//! End-to-end tests over a help file assembled byte by byte.
//!
//! The unit tests each cover one layer. These build a whole file, with a real header,
//! directory, `|SYSTEM`, `|TOPIC` and `|CONTEXT`, and read it back through the public API,
//! so a mistake in how the layers fit together shows up here rather than only against a
//! file nobody can commit to the repository.

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use libhlp::{HlpError, HlpFile, Run, Target, hash};

const MAGIC: u32 = 0x0003_5F3F;
const BTREE_MAGIC: u16 = 0x293B;
const SYSTEM_MAGIC: u16 = 0x036C;
const BLOCK_HEADER_LEN: usize = 12;
const LINK_LEN: usize = 21;
const TOPIC_HEADER_LEN: usize = 28;
const PAGE_SIZE: usize = 1024;
const BLOCK_SIZE: usize = 4096;

/// Wrap `content` in the `FILEHEADER` that precedes every internal file.
fn internal_file(content: &[u8]) -> Vec<u8> {
	let mut buf = Vec::new();
	let reserved = content.len() + 9;
	buf.extend_from_slice(&(reserved as u32).to_le_bytes());
	buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
	buf.push(4);
	buf.extend_from_slice(content);
	buf
}

/// Build a single-leaf B+ tree whose entries are already encoded.
fn btree(entries: &[Vec<u8>]) -> Vec<u8> {
	let mut page = Vec::new();
	page.extend_from_slice(&0u16.to_le_bytes());
	page.extend_from_slice(&(entries.len() as i16).to_le_bytes());
	page.extend_from_slice(&(-1i16).to_le_bytes());
	page.extend_from_slice(&(-1i16).to_le_bytes());
	for entry in entries {
		page.extend_from_slice(entry);
	}
	page.resize(PAGE_SIZE, 0);
	let mut buf = Vec::new();
	buf.extend_from_slice(&BTREE_MAGIC.to_le_bytes());
	buf.extend_from_slice(&0x0402u16.to_le_bytes());
	buf.extend_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
	buf.extend_from_slice(&[0u8; 16]);
	buf.extend_from_slice(&0i16.to_le_bytes());
	buf.extend_from_slice(&0i16.to_le_bytes());
	buf.extend_from_slice(&0i16.to_le_bytes());
	buf.extend_from_slice(&(-1i16).to_le_bytes());
	buf.extend_from_slice(&1u16.to_le_bytes());
	buf.extend_from_slice(&1u16.to_le_bytes());
	buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
	buf.extend_from_slice(&page);
	buf
}

/// A `|SYSTEM` file for an uncompressed WinHelp 3.1 help file with the given title.
fn system_file(title: &str) -> Vec<u8> {
	let mut buf = Vec::new();
	buf.extend_from_slice(&SYSTEM_MAGIC.to_le_bytes());
	buf.extend_from_slice(&21u16.to_le_bytes());
	buf.extend_from_slice(&1u16.to_le_bytes());
	buf.extend_from_slice(&0u32.to_le_bytes());
	// Flags 0: topic blocks are stored as they stand.
	buf.extend_from_slice(&0u16.to_le_bytes());
	buf.extend_from_slice(&1u16.to_le_bytes());
	buf.extend_from_slice(&((title.len() + 1) as u16).to_le_bytes());
	buf.extend_from_slice(title.as_bytes());
	buf.push(0);
	buf
}

/// One record of `|TOPIC`: its header, then both data streams.
fn record(kind: u8, next: u32, data1: &[u8], data2: &[u8]) -> Vec<u8> {
	let data_len1 = (LINK_LEN + data1.len()) as u32;
	let block_size = data_len1 + data2.len() as u32;
	let mut buf = Vec::new();
	buf.extend_from_slice(&block_size.to_le_bytes());
	buf.extend_from_slice(&(data2.len() as u32).to_le_bytes());
	buf.extend_from_slice(&0u32.to_le_bytes());
	buf.extend_from_slice(&next.to_le_bytes());
	buf.extend_from_slice(&data_len1.to_le_bytes());
	buf.push(kind);
	buf.extend_from_slice(data1);
	buf.extend_from_slice(data2);
	buf
}

/// What one record holds, before its position in the chain is known.
enum Part {
	Header(String),
	Display(Vec<u8>, Vec<u8>),
}

/// Chain records together and wrap them in one topic block.
fn topic_file(parts: &[Part]) -> Vec<u8> {
	let encoded: Vec<(u8, Vec<u8>, Vec<u8>)> = parts
		.iter()
		.map(|part| match part {
			Part::Header(title) => {
				let mut data2 = title.as_bytes().to_vec();
				data2.push(0);
				(0x02, vec![0u8; TOPIC_HEADER_LEN], data2)
			}
			Part::Display(data1, data2) => (0x20, data1.clone(), data2.clone()),
		})
		.collect();
	let mut pos = BLOCK_HEADER_LEN as u32;
	let mut starts = Vec::new();
	for (_, data1, data2) in &encoded {
		starts.push(pos);
		pos += (LINK_LEN + data1.len() + data2.len()) as u32;
	}
	let mut payload = Vec::new();
	for (i, (kind, data1, data2)) in encoded.iter().enumerate() {
		let next = starts.get(i + 1).copied().unwrap_or(pos);
		payload.extend_from_slice(&record(*kind, next, data1, data2));
	}
	// Every file ends with an empty record pointing nowhere.
	payload.extend_from_slice(&record(0, 0, &[], &[]));
	let mut block = vec![0u8; BLOCK_HEADER_LEN];
	block.extend_from_slice(&payload);
	block.resize(BLOCK_SIZE, 0);
	block
}

/// `LinkData1` for a display record: prologue, empty settings, then `commands`.
fn display_commands(topic_length: u16, commands: &[u8]) -> Vec<u8> {
	let mut buf = Vec::new();
	// TopicSize as a compressed long of zero, then TopicLength.
	buf.extend_from_slice(&0x8000u16.to_le_bytes());
	buf.push((topic_length * 2) as u8);
	buf.extend_from_slice(&[0, 0, 0, 0]);
	buf.extend_from_slice(&0u16.to_le_bytes());
	buf.extend_from_slice(commands);
	buf
}

fn strings(parts: &[&str]) -> Vec<u8> {
	let mut buf = Vec::new();
	for part in parts {
		buf.extend_from_slice(part.as_bytes());
		buf.push(0);
	}
	buf
}

/// Assemble a whole help file from named internal files.
fn build(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
	// Lay the internal files out after the header, noting where each one lands so the
	// directory can point at them.
	let mut body = Vec::new();
	let mut entries = Vec::new();
	for (name, content) in files {
		entries.push((*name, 16 + body.len() as u32));
		body.extend_from_slice(&internal_file(content));
	}
	entries.sort_by_key(|(name, _)| *name);
	let dir_entries: Vec<Vec<u8>> = entries
		.iter()
		.map(|(name, offset)| {
			let mut e = name.as_bytes().to_vec();
			e.push(0);
			e.extend_from_slice(&offset.to_le_bytes());
			e
		})
		.collect();
	let directory = internal_file(&btree(&dir_entries));
	let directory_start = 16 + body.len() as u32;
	let total = 16 + body.len() + directory.len();
	let mut buf = Vec::new();
	buf.extend_from_slice(&MAGIC.to_le_bytes());
	buf.extend_from_slice(&directory_start.to_le_bytes());
	buf.extend_from_slice(&u32::MAX.to_le_bytes());
	buf.extend_from_slice(&(total as u32).to_le_bytes());
	buf.extend_from_slice(&body);
	buf.extend_from_slice(&directory);
	buf
}

/// A `|CONTEXT` tree mapping each context string to a topic offset.
fn context_file(entries: &[(&str, u32)]) -> Vec<u8> {
	let mut rows: Vec<(u32, u32)> = entries.iter().map(|(name, off)| (hash(name), *off)).collect();
	rows.sort_unstable();
	let encoded: Vec<Vec<u8>> = rows
		.iter()
		.map(|(h, off)| {
			let mut e = h.to_le_bytes().to_vec();
			e.extend_from_slice(&off.to_le_bytes());
			e
		})
		.collect();
	btree(&encoded)
}

#[test]
fn a_whole_file_reads_back_its_topics() {
	let topic = topic_file(&[
		Part::Header("Contents".into()),
		Part::Display(display_commands(9, &[0x82, 0xFF]), strings(&["First.", "Second."])),
		Part::Header("Details".into()),
		Part::Display(display_commands(6, &[0xFF]), strings(&["Body."])),
	]);
	let hlp = HlpFile::from_bytes(build(&[("|SYSTEM", system_file("Test Help")), ("|TOPIC", topic)])).unwrap();
	assert_eq!(hlp.title(), Some("Test Help"));
	assert_eq!(hlp.version(), 21);
	let topics = hlp.topics().unwrap();
	assert_eq!(topics.len(), 2);
	assert_eq!(topics[0].title, "Contents");
	assert_eq!(topics[0].text(), "First.\nSecond.");
	assert_eq!(topics[1].title, "Details");
	assert_eq!(topics[1].text(), "Body.");
	// Topic numbers count from 16, as WinHelp does.
	assert_eq!(topics[0].number, 16);
	assert_eq!(topics[1].number, 17);
}

// A link stores the hash of its target's context string, so reading one back has to go
// through |CONTEXT to land on a topic. This is the step that is easiest to get subtly
// wrong, since a hash is a plausible looking number on its own.
#[test]
fn a_hotspot_resolves_through_the_context_index() {
	let mut jump = vec![0xE3u8];
	jump.extend_from_slice(&hash("details").to_le_bytes());
	jump.push(0x89);
	jump.push(0xFF);
	let topic = topic_file(&[
		Part::Header("Contents".into()),
		Part::Display(display_commands(20, &jump), strings(&["See ", "the details", " below."])),
		Part::Header("Details".into()),
		Part::Display(display_commands(6, &[0xFF]), strings(&["Body."])),
	]);
	let files = vec![
		("|SYSTEM", system_file("Test Help")),
		("|TOPIC", topic),
		("|CONTEXT", context_file(&[("details", 20)])),
	];
	let hlp = HlpFile::from_bytes(build(&files)).unwrap();
	let topics = hlp.topics().unwrap();
	let link = topics[0].links().next().expect("the first topic has a hotspot");
	assert_eq!(link.target, Target::Context(hash("details")));
	assert!(!link.popup);
	assert_eq!(hlp.topic_offset(link), Some(20));
	assert_eq!(topics[1].offset, 20);
	// Only the hotspot's own text carries the link.
	let runs = &topics[0].paragraphs[0].runs;
	assert_eq!(runs[0], Run::Text { text: "See ".into(), link: None });
	assert!(matches!(&runs[1], Run::Text { text, link: Some(_) } if text == "the details"));
	assert_eq!(runs[2], Run::Text { text: " below.".into(), link: None });
}

#[test]
fn internal_files_are_listed_and_readable() {
	let topic = topic_file(&[Part::Header("T".into())]);
	let files = vec![
		("|SYSTEM", system_file("Test Help")),
		("|TOPIC", topic),
		("|bm0", b"not really a bitmap".to_vec()),
	];
	let hlp = HlpFile::from_bytes(build(&files)).unwrap();
	let mut names: Vec<&str> = hlp.file_names().collect();
	names.sort_unstable();
	assert_eq!(names, ["|SYSTEM", "|TOPIC", "|bm0"]);
	assert_eq!(hlp.read_file("|bm0").unwrap(), b"not really a bitmap");
	assert!(matches!(hlp.read_file("|nope"), Err(HlpError::NotFound(_))));
}

// A file with no |TOPIC holds no help text, and should say so plainly rather than
// pretending it has zero topics.
#[test]
fn a_file_with_no_topic_file_reports_it() {
	let hlp = HlpFile::from_bytes(build(&[("|SYSTEM", system_file("Empty"))])).unwrap();
	assert!(matches!(hlp.topics(), Err(HlpError::NotFound(_))));
}

#[test]
fn a_file_with_no_system_file_is_rejected() {
	let topic = topic_file(&[Part::Header("T".into())]);
	assert!(matches!(HlpFile::from_bytes(build(&[("|TOPIC", topic)])), Err(HlpError::NotFound(_))));
}
