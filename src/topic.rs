//! Walking `|TOPIC`, the internal file that holds every topic's text.
//!
//! `|TOPIC` is a run of fixed size blocks, each with a small header and a payload that may
//! be LZ77 compressed. Positions into it are not plain file offsets: a topic position
//! addresses a byte of the *decompressed* payload, with every block given the same stride
//! whether or not it fills it. [`Blocks`] presents that address space as one stream, and
//! [`walk`] follows the linked list of records laid out along it.

use crate::{
	error::{HlpError, Result},
	font::Fonts,
	format::Reader,
	link::Link,
	lz77,
	paragraph::{self, Paragraph, TL_DISPLAY, TL_DISPLAY30, TL_TABLE, TL_TOPICHDR},
	phrase::Phrases,
	table::Block,
	text,
};

/// Bytes in a `TOPICBLOCKHEADER`, and so the position the first record sits at.
const BLOCK_HEADER_LEN: usize = 12;

/// Bytes in the header of one record.
const LINK_LEN: usize = 21;

/// Bytes in a WinHelp 3.1 `TOPICHEADER`, the shortest a 3.1 header record's data can be.
const TOPIC_HEADER_LEN: usize = 28;

/// Bytes in a WinHelp 3.0 `TOPICHEADER`, which carries only a size and two topic numbers.
const TOPIC_HEADER_LEN_30: usize = 12;

/// Topic numbers start here rather than at zero, matching what `|TOMAP` indexes.
const FIRST_TOPIC_NUMBER: u32 = 16;

/// Topic offsets advance by this much per block, independent of the block's byte size.
const OFFSET_PER_BLOCK: u32 = 0x8000;

/// Records to follow before giving up, in case a file's linked list loops.
const MAX_RECORDS: usize = 1 << 22;

/// One topic: a screen of help, its title, and where other topics link to it.
#[derive(Debug, Clone)]
pub struct Topic {
	/// Sequential topic number, counting from 16 as WinHelp does.
	pub number: u32,
	/// Topic offset that identifies this topic as a link target.
	///
	/// Only WinHelp 3.1 and later track this. A WinHelp 3.0 file has no topic offsets at
	/// all, so every topic in one reports zero; use [`Self::number`] to tell them apart.
	pub offset: u32,
	/// Title from the `$` footnote, empty if the author set none.
	pub title: String,
	/// The topic's contents, in reading order.
	pub blocks: Vec<Block>,
}

impl Topic {
	/// The topic's whole text, one block per line.
	#[must_use]
	pub fn text(&self) -> String {
		self.blocks.iter().map(Block::text).collect::<Vec<_>>().join("\n")
	}

	/// Every paragraph in the topic, table cells included, in reading order.
	pub fn paragraphs(&self) -> impl Iterator<Item = &Paragraph> {
		self.blocks.iter().flat_map(Block::paragraphs)
	}

	/// Every hotspot in the topic, in reading order.
	pub fn links(&self) -> impl Iterator<Item = &Link> {
		self.paragraphs().flat_map(Paragraph::links)
	}
}

/// The decompressed payload of every topic block, addressed by topic position.
pub struct Blocks {
	payloads: Vec<Vec<u8>>,
	stride: u32,
}

impl Blocks {
	/// Split `raw` into blocks and decompress each one's payload.
	pub fn new(raw: &[u8], block_size: usize, decompress_size: usize, compressed: bool) -> Self {
		let stride = u32::try_from(decompress_size).unwrap_or(u32::MAX);
		let payloads = raw
			.chunks(block_size.max(BLOCK_HEADER_LEN + 1))
			.map(|block| {
				// The header names neighbouring records, all of which the walk reaches by
				// following the records themselves, so only the payload is needed here.
				let body = block.get(BLOCK_HEADER_LEN..).unwrap_or_default();
				if compressed { lz77::decompress(body, decompress_size) } else { body.to_vec() }
			})
			.collect();
		Self { payloads, stride }
	}

	/// Read `len` bytes starting at topic position `pos`.
	///
	/// Returns the bytes and the position just past them. A read that runs off the end of
	/// a block continues at the start of the next, which is why the caller must use the
	/// returned position rather than adding `len` itself.
	pub fn read(&self, pos: u32, len: usize) -> Option<(Vec<u8>, u32)> {
		let mut pos = pos;
		let mut left = len;
		let mut out = Vec::with_capacity(len);
		loop {
			let rel = pos.checked_sub(u32::try_from(BLOCK_HEADER_LEN).ok()?)?;
			let index = (rel / self.stride) as usize;
			let offset = (rel % self.stride) as usize;
			let payload = self.payloads.get(index)?;
			let available = payload.len().saturating_sub(offset);
			if available >= left {
				out.extend_from_slice(&payload[offset..offset + left]);
				return Some((out, pos + u32::try_from(left).ok()?));
			}
			out.extend_from_slice(payload.get(offset..).unwrap_or_default());
			left -= available;
			// The next block's slot starts one full stride on, no matter how much of this
			// block was actually used.
			pos = u32::try_from(index + 1)
				.ok()?
				.checked_mul(self.stride)?
				.checked_add(u32::try_from(BLOCK_HEADER_LEN).ok()?)?;
		}
	}
}

/// The header of one record in `|TOPIC`.
struct TopicLink {
	/// Size of the header plus both data streams.
	block_size: u32,
	/// Size `LinkData2` expands to.
	data_len2: u32,
	/// Position of the next record, or 0 or -1 at the end of the file.
	next: u32,
	/// Size of the header plus `LinkData1`.
	data_len1: u32,
	record_type: u8,
}

impl TopicLink {
	fn parse(buf: &[u8]) -> Result<Self> {
		let mut r = Reader::new(buf);
		let bad = |_| HlpError::BadTopic("truncated record header");
		let block_size = r.u32().map_err(bad)?;
		let data_len2 = r.u32().map_err(bad)?;
		// PrevBlock, which the forward walk never needs.
		r.skip(4).map_err(bad)?;
		let next = r.u32().map_err(bad)?;
		let data_len1 = r.u32().map_err(bad)?;
		let record_type = r.u8().map_err(bad)?;
		Ok(Self { block_size, data_len2, next, data_len1, record_type })
	}
}

/// Read every topic in the file.
///
/// A record that cannot be read ends the walk, keeping the topics gathered so far, since a
/// help file with one damaged topic is still worth the rest of its text.
pub fn walk(blocks: &Blocks, phrases: &Phrases, fonts: &Fonts, before31: bool) -> Vec<Topic> {
	let header_len = if before31 { TOPIC_HEADER_LEN_30 } else { TOPIC_HEADER_LEN };
	let mut topics = Vec::new();
	let mut current: Option<Topic> = None;
	let mut pos = u32::try_from(BLOCK_HEADER_LEN).unwrap_or(0);
	let mut offset = 0u32;
	let mut number = FIRST_TOPIC_NUMBER;
	for _ in 0..MAX_RECORDS {
		let Some((header, after_header)) = blocks.read(pos, LINK_LEN) else { break };
		let Ok(link) = TopicLink::parse(&header) else { break };
		// The last record carries no data and points nowhere.
		if link.next == 0 || link.next == u32::MAX {
			break;
		}
		// WinHelp 3.0 stores the distance to the next record; 3.1 stores its position. A
		// 3.1 pointer that does not move forward would loop, so it ends the walk too.
		let next = if before31 { pos.saturating_add(link.next) } else { link.next };
		if next <= pos {
			break;
		}
		let Some((data1, after_data1)) = read_stream(blocks, after_header, link.data_len1, LINK_LEN) else { break };
		let stored2 = link.block_size.saturating_sub(link.data_len1);
		let Some((raw2, _)) = read_stream(blocks, after_data1, link.block_size, link.data_len1 as usize) else {
			break;
		};
		// LinkData2 is phrase compressed exactly when it expands to more than it occupies.
		let data2 = if link.data_len2 > stored2 {
			phrases.expand(&raw2, link.data_len2 as usize)
		} else {
			raw2.into_iter().take(link.data_len2 as usize).collect()
		};
		match link.record_type {
			// A header record too short to hold a TOPICHEADER is not one, whatever it
			// claims, and treating it as one would end the topic that is open.
			TL_TOPICHDR if data1.len() >= header_len => {
				topics.extend(current.take());
				current =
					Some(Topic { number, offset, title: text::decode_cstr(topic_title(&data2)), blocks: Vec::new() });
				number += 1;
			}
			TL_DISPLAY30 | TL_DISPLAY | TL_TABLE => {
				let record = paragraph::parse(link.record_type, &data1, &data2, fonts);
				offset = offset.saturating_add(record.topic_length);
				if let Some(topic) = current.as_mut() {
					append_blocks(&mut topic.blocks, record.blocks);
				}
			}
			_ => {}
		}
		offset = next_offset(offset, pos, next, blocks.stride);
		pos = next;
	}
	topics.extend(current);
	topics
}

/// Add a record's blocks to a topic, joining a table onto the one before it.
///
/// The help compiler emits one record per table row, so a ten row table arrives as ten
/// one-row tables. Rejoining them here is what makes a table read as a table.
fn append_blocks(blocks: &mut Vec<Block>, incoming: Vec<Block>) {
	for block in incoming {
		match (blocks.last_mut(), block) {
			(Some(Block::Table(open)), Block::Table(next)) => open.rows.extend(next.rows),
			(_, block) => blocks.push(block),
		}
	}
}

/// Read one of a record's two data streams, whose stored length is `total` less `header`.
fn read_stream(blocks: &Blocks, pos: u32, total: u32, header: usize) -> Option<(Vec<u8>, u32)> {
	let len = (total as usize).saturating_sub(header);
	if len == 0 {
		return Some((Vec::new(), pos));
	}
	blocks.read(pos, len)
}

/// The title is the first string of a topic header's `LinkData2`; the rest are macros.
fn topic_title(data2: &[u8]) -> &[u8] {
	let end = data2.iter().position(|&b| b == 0).unwrap_or(data2.len());
	&data2[..end]
}

/// Advance the topic offset counter when moving to a record in a later block.
///
/// Topic offsets are counted per block, so crossing into a new block snaps the counter to
/// that block's base rather than carrying the character count over.
// The header length is 12, which fits every integer type in sight. It is declared as a
// length because that is what most of this module needs it as, and converted here because
// this one calculation works in positions.
#[allow(clippy::cast_possible_truncation)]
const fn next_offset(offset: u32, pos: u32, next: u32, stride: u32) -> u32 {
	let header = BLOCK_HEADER_LEN as u32;
	if pos < header || next < header || stride == 0 {
		return offset;
	}
	let from = (pos - header) / stride;
	let to = (next - header) / stride;
	if from == to { offset } else { to.saturating_mul(OFFSET_PER_BLOCK) }
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

	use super::*;

	/// A stride wide enough that no test payload reaches it, as in a real file where the
	/// stride is 16k but a block's payload is at most about 4k.
	const STRIDE: usize = 0x4000;

	/// Build a `|TOPIC` file of uncompressed blocks holding `payloads`.
	fn topic_file(payloads: &[Vec<u8>], block_size: usize) -> Vec<u8> {
		let mut buf = Vec::new();
		for payload in payloads {
			let mut block = vec![0u8; BLOCK_HEADER_LEN];
			block.extend_from_slice(payload);
			block.resize(block_size, 0);
			buf.extend_from_slice(&block);
		}
		buf
	}

	fn blocks_with_stride(payloads: &[Vec<u8>], block_size: usize, stride: usize) -> Blocks {
		Blocks::new(&topic_file(payloads, block_size), block_size, stride, false)
	}

	fn blocks(payloads: &[Vec<u8>], block_size: usize) -> Blocks {
		blocks_with_stride(payloads, block_size, STRIDE)
	}

	#[test]
	fn a_read_inside_one_block_returns_the_bytes_and_the_next_position() {
		let b = blocks(&[b"abcdef".to_vec()], BLOCK_HEADER_LEN + 64);
		let (bytes, next) = b.read(12, 3).unwrap();
		assert_eq!(bytes, b"abc");
		assert_eq!(next, 15);
	}

	// A record can straddle a block boundary, and the position after it is the start of
	// the next block's slot, not the position plus the length.
	#[test]
	fn a_read_crossing_a_block_continues_in_the_next_one() {
		let b = blocks_with_stride(&[vec![b'a'; 64], b"XY".to_vec()], BLOCK_HEADER_LEN + 64, 64);
		let (bytes, next) = b.read(12 + 64 - 2, 4).unwrap();
		assert_eq!(bytes, b"aaXY");
		assert_eq!(next, 64 + 12 + 2);
	}

	// A compressed block expands to far less than the stride, so a read running past what
	// it holds must jump to the next block's base rather than into the unused gap. This is
	// the ordinary case in a real file: the stride is 16k and a block expands to about 4k.
	#[test]
	fn a_read_past_a_short_payload_jumps_to_the_next_block() {
		let mut raw = vec![0u8; BLOCK_HEADER_LEN];
		raw.extend_from_slice(&[0b000, b'a', b'b']);
		raw.extend_from_slice(&[0u8; BLOCK_HEADER_LEN]);
		raw.extend_from_slice(&[0b000, b'c', b'd']);
		let b = Blocks::new(&raw, BLOCK_HEADER_LEN + 3, 1024, true);
		let (bytes, next) = b.read(12, 4).unwrap();
		assert_eq!(bytes, b"abcd");
		assert_eq!(next, 1024 + 12 + 2);
	}

	#[test]
	fn a_read_past_the_last_block_fails() {
		let b = blocks(&[b"abc".to_vec()], BLOCK_HEADER_LEN + 64);
		assert!(b.read(12, 99).is_none());
		assert!(b.read(0, 1).is_none());
	}

	#[test]
	fn compressed_blocks_are_expanded() {
		// One LZ77 group: three literals.
		let mut block = vec![0u8; BLOCK_HEADER_LEN];
		block.extend_from_slice(&[0b000, b'a', b'b', b'c']);
		let b = Blocks::new(&block, block.len(), STRIDE, true);
		assert_eq!(b.read(12, 3).unwrap().0, b"abc");
	}

	/// Assemble one record: header, `LinkData1`, then `LinkData2`.
	fn record(record_type: u8, next: u32, data1: &[u8], data2: &[u8], data_len2: u32) -> Vec<u8> {
		let data_len1 = (LINK_LEN + data1.len()) as u32;
		let block_size = data_len1 + data2.len() as u32;
		let mut buf = Vec::new();
		buf.extend_from_slice(&block_size.to_le_bytes());
		buf.extend_from_slice(&data_len2.to_le_bytes());
		buf.extend_from_slice(&0u32.to_le_bytes());
		buf.extend_from_slice(&next.to_le_bytes());
		buf.extend_from_slice(&data_len1.to_le_bytes());
		buf.push(record_type);
		buf.extend_from_slice(data1);
		buf.extend_from_slice(data2);
		buf
	}

	fn topic_header_parts(title: &str) -> (u8, Vec<u8>, Vec<u8>) {
		let mut data2 = title.as_bytes().to_vec();
		data2.push(0);
		(TL_TOPICHDR, vec![0u8; TOPIC_HEADER_LEN], data2)
	}

	fn display_parts(text: &str) -> (u8, Vec<u8>, Vec<u8>) {
		// TopicSize, TopicLength, then a settings block with no optional fields and one
		// end-of-formatting command.
		let mut data1 = Vec::new();
		data1.extend_from_slice(&0x8000u16.to_le_bytes());
		data1.push(0);
		data1.extend_from_slice(&[0, 0, 0, 0]);
		data1.extend_from_slice(&0u16.to_le_bytes());
		data1.push(0xFF);
		let mut data2 = text.as_bytes().to_vec();
		data2.push(0);
		(TL_DISPLAY, data1, data2)
	}

	/// Lay records out end to end, chaining each to the next and closing the list with the
	/// empty terminating record every real file ends with.
	fn chain(parts: &[(u8, Vec<u8>, Vec<u8>)]) -> Vec<u8> {
		let mut pos = BLOCK_HEADER_LEN as u32;
		let mut starts = Vec::new();
		for (_, data1, data2) in parts {
			starts.push(pos);
			pos += (LINK_LEN + data1.len() + data2.len()) as u32;
		}
		let mut out = Vec::new();
		for (i, (kind, data1, data2)) in parts.iter().enumerate() {
			let next = starts.get(i + 1).copied().unwrap_or(pos);
			out.extend_from_slice(&record(*kind, next, data1, data2, data2.len() as u32));
		}
		out.extend_from_slice(&record(0, 0, &[], &[], 0));
		out
	}

	#[test]
	fn a_topic_header_followed_by_text_makes_one_topic() {
		let payload = chain(&[topic_header_parts("Intro"), display_parts("Body text.")]);
		let b = blocks(&[payload], BLOCK_HEADER_LEN + 512);
		let topics = walk(&b, &Phrases::default(), &Fonts::default(), false);
		assert_eq!(topics.len(), 1);
		assert_eq!(topics[0].title, "Intro");
		assert_eq!(topics[0].number, FIRST_TOPIC_NUMBER);
		assert_eq!(topics[0].text(), "Body text.");
	}

	#[test]
	fn each_topic_header_starts_a_new_topic() {
		let payload = chain(&[
			topic_header_parts("First"),
			display_parts("One"),
			topic_header_parts("Second"),
			display_parts("Two"),
		]);
		let b = blocks(&[payload], BLOCK_HEADER_LEN + 1024);
		let topics = walk(&b, &Phrases::default(), &Fonts::default(), false);
		assert_eq!(topics.len(), 2);
		assert_eq!(topics[0].title, "First");
		assert_eq!(topics[0].text(), "One");
		assert_eq!(topics[1].title, "Second");
		assert_eq!(topics[1].text(), "Two");
		assert_eq!(topics[1].number, FIRST_TOPIC_NUMBER + 1);
	}

	// Several paragraphs of one topic can be spread over more than one record, and they
	// have to land in the topic that is open rather than starting a new one.
	#[test]
	fn several_records_accumulate_into_one_topic() {
		let payload = chain(&[topic_header_parts("T"), display_parts("First."), display_parts("Second.")]);
		let b = blocks(&[payload], BLOCK_HEADER_LEN + 512);
		let topics = walk(&b, &Phrases::default(), &Fonts::default(), false);
		assert_eq!(topics.len(), 1);
		assert_eq!(
			topics[0].text(),
			"First.
Second."
		);
	}

	// The terminating record carries no data, so a file that is nothing but a terminator
	// has no topics rather than one empty one.
	#[test]
	fn the_terminating_record_is_not_a_topic() {
		let payload = chain(&[]);
		let b = blocks(&[payload], BLOCK_HEADER_LEN + 64);
		assert!(walk(&b, &Phrases::default(), &Fonts::default(), false).is_empty());
	}

	/// Chain records the WinHelp 3.0 way: each names the distance to the next rather than
	/// its position, and a topic header carries only the shorter 3.0 structure.
	fn chain_30(parts: &[(u8, Vec<u8>, Vec<u8>)]) -> Vec<u8> {
		let mut out = Vec::new();
		for (kind, data1, data2) in parts {
			let len = (LINK_LEN + data1.len() + data2.len()) as u32;
			out.extend_from_slice(&record(*kind, len, data1, data2, data2.len() as u32));
		}
		out.extend_from_slice(&record(0, 0, &[], &[], 0));
		out
	}

	// WinHelp 3.0 stores the distance to the next record where 3.1 stores its position.
	// Reading one as the other walks off into the middle of the file immediately.
	#[test]
	fn a_30_file_follows_distances_rather_than_positions() {
		let payload = chain_30(&[
			(TL_TOPICHDR, vec![0u8; TOPIC_HEADER_LEN_30], b"First ".to_vec()),
			(TL_DISPLAY30, display_30_commands(), b"Body. ".to_vec()),
			(TL_TOPICHDR, vec![0u8; TOPIC_HEADER_LEN_30], b"Second ".to_vec()),
		]);
		let b = blocks(&[payload], BLOCK_HEADER_LEN + 512);
		let topics = walk(&b, &Phrases::default(), &Fonts::default(), true);
		assert_eq!(topics.len(), 2);
		assert_eq!(topics[0].title, "First");
		assert_eq!(topics[0].text(), "Body.");
		assert_eq!(topics[1].title, "Second");
	}

	// A 3.0 topic header is twelve bytes, not twenty-eight. Holding out for the longer one
	// would reject every header in the file and leave the walk with no topics at all.
	#[test]
	fn a_30_topic_header_is_accepted_at_its_own_length() {
		let parts = vec![
			(TL_TOPICHDR, vec![0u8; TOPIC_HEADER_LEN_30], b"Short ".to_vec()),
			(TL_DISPLAY30, display_30_commands(), b"Text. ".to_vec()),
		];
		let b = blocks(&[chain_30(&parts)], BLOCK_HEADER_LEN + 512);
		assert_eq!(walk(&b, &Phrases::default(), &Fonts::default(), true).len(), 1);
		// The same file read as 3.1 finds no header long enough to start a topic.
		assert!(walk(&b, &Phrases::default(), &Fonts::default(), false).is_empty());
	}

	/// `LinkData1` for a 3.0 display record, which has no topic length field.
	fn display_30_commands() -> Vec<u8> {
		let mut buf = Vec::new();
		buf.extend_from_slice(&0x8000u16.to_le_bytes());
		buf.extend_from_slice(&[0, 0, 0, 0]);
		buf.extend_from_slice(&0u16.to_le_bytes());
		buf.push(0xFF);
		buf
	}

	// A next pointer that does not advance would spin forever on a damaged file.
	#[test]
	fn a_record_pointing_at_itself_ends_the_walk() {
		let payload = record(TL_TOPICHDR, BLOCK_HEADER_LEN as u32, &[0u8; TOPIC_HEADER_LEN], b"Loop ", 5);
		let b = blocks(&[payload], BLOCK_HEADER_LEN + 256);
		assert!(walk(&b, &Phrases::default(), &Fonts::default(), false).is_empty());
	}

	#[test]
	fn an_empty_topic_file_yields_no_topics() {
		let b = Blocks::new(&[], 512, STRIDE, false);
		assert!(walk(&b, &Phrases::default(), &Fonts::default(), false).is_empty());
	}

	#[test]
	fn crossing_a_block_snaps_the_topic_offset_to_the_new_block() {
		assert_eq!(next_offset(5, 12, 20, 64), 5);
		assert_eq!(next_offset(5, 12, 12 + 64, 64), OFFSET_PER_BLOCK);
		assert_eq!(next_offset(5, 12, 12 + 128, 64), 2 * OFFSET_PER_BLOCK);
	}
}
