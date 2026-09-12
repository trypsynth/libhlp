//! The public entry point: opening a help file and reading topics out of it.

use std::{collections::HashMap, fmt, fs, path::Path};

use crate::{
	btree::Btree,
	error::{HlpError, Result},
	format::{self, Reader},
	link::{Link, Target},
	phrase::Phrases,
	system::System,
	text,
	topic::{self, Blocks, Topic},
};

/// What the internal directory is called in errors. It is found by offset from the file
/// header rather than by name, so it has no name of its own.
const DIRECTORY: &str = "internal directory";

/// The internal file describing the file's format and compression.
const SYSTEM: &str = "|SYSTEM";

/// The internal file holding topic text.
const TOPIC: &str = "|TOPIC";

/// The internal file holding an old-style phrase table.
const PHRASES: &str = "|Phrases";

/// The internal file holding Hall phrase lengths.
const PHRINDEX: &str = "|PhrIndex";

/// The internal file holding Hall phrase text.
const PHRIMAGE: &str = "|PhrImage";

/// The internal file mapping topic offsets to the titles shown in the search dialog.
const TITLES: &str = "|TTLBTREE";

/// The internal file mapping context hashes to the topic offsets they name.
const CONTEXT: &str = "|CONTEXT";

/// An opened WinHelp file.
///
/// The whole file is read into memory when it is opened. Help files are small by modern
/// standards, and every position in `|TOPIC` is an offset into decompressed data that has
/// to be reconstructed anyway, so streaming would buy nothing.
pub struct HlpFile {
	data: Vec<u8>,
	directory: HashMap<String, u32>,
	system: System,
	contexts: HashMap<u32, u32>,
}

impl fmt::Debug for HlpFile {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("HlpFile")
			.field("size", &self.data.len())
			.field("version", &self.system.version)
			.field("files", &self.directory.len())
			.field("title", &self.system.title)
			.finish_non_exhaustive()
	}
}

impl HlpFile {
	/// Open the help file at `path`.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadMagic`] if the file is not a WinHelp file at all, and
	/// [`HlpError::UnsupportedVersion`] for a WinHelp 3.0 file, which this crate does not
	/// read yet.
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		Self::from_bytes(fs::read(path)?)
	}

	/// Open a help file already in memory.
	///
	/// # Errors
	///
	/// As [`Self::open`].
	pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
		let header = format::parse_file_header(&data)?;
		// Catching a short file here turns what would otherwise surface as a confusing bad
		// B+ tree deep in the directory walk into the plain answer: bytes are missing.
		if (data.len() as u64) < u64::from(header.entire_file_size) {
			return Err(HlpError::Truncated);
		}
		let directory = read_directory(&data, header.directory_start)?;
		let system_offset = *directory.get(SYSTEM).ok_or_else(|| HlpError::NotFound(SYSTEM.into()))?;
		let system = System::parse(format::internal_file_at(&data, system_offset)?)?;
		let mut hlp = Self { data, directory, system, contexts: HashMap::new() };
		hlp.contexts = hlp.read_contexts();
		Ok(hlp)
	}

	/// The help file's title, as the author set it in the help project file.
	#[must_use]
	pub fn title(&self) -> Option<&str> {
		self.system.title.as_deref()
	}

	/// The copyright notice WinHelp would show in its About box.
	#[must_use]
	pub fn copyright(&self) -> Option<&str> {
		self.system.copyright.as_deref()
	}

	/// The format version: 21 for `HC31`, 27 for media view, 33 for HCW 4.00.
	#[must_use]
	pub const fn version(&self) -> u16 {
		self.system.version
	}

	/// The Windows character set the file declares, if it declares one.
	///
	/// Text is decoded as Windows-1252 regardless. A caller that needs another code page
	/// can re-decode the bytes from [`Self::read_file`].
	#[must_use]
	pub const fn charset(&self) -> Option<u16> {
		self.system.charset
	}

	/// Topic offset of the topic WinHelp opens the file on.
	#[must_use]
	pub const fn contents_offset(&self) -> Option<u32> {
		self.system.contents
	}

	/// The names of every internal file, in no particular order.
	pub fn file_names(&self) -> impl Iterator<Item = &str> {
		self.directory.keys().map(String::as_str)
	}

	/// The raw bytes of one internal file, such as `|SYSTEM` or `|bm0`.
	///
	/// # Errors
	///
	/// Returns [`HlpError::NotFound`] if the file has no such entry.
	pub fn read_file(&self, name: &str) -> Result<&[u8]> {
		let offset = *self.directory.get(name).ok_or_else(|| HlpError::NotFound(name.to_string()))?;
		format::internal_file_at(&self.data, offset)
	}

	/// Read every topic in the file, in the order they are stored.
	///
	/// # Errors
	///
	/// Returns [`HlpError::NotFound`] if the file has no `|TOPIC`, which means it holds no
	/// help text at all.
	pub fn topics(&self) -> Result<Vec<Topic>> {
		let raw = self.read_file(TOPIC)?;
		let blocks =
			Blocks::new(raw, self.system.block_size, self.system.decompress_size, self.system.compressed);
		let phrases = self.phrases();
		let mut topics = topic::walk(&blocks, &phrases);
		self.apply_stored_titles(&mut topics);
		Ok(topics)
	}

	/// Load whichever phrase table the file uses, if it uses one.
	///
	/// A file with no phrase table, or one this crate cannot make sense of, reads as no
	/// substitutions at all: its topic text is then stored plainly and comes out intact.
	fn phrases(&self) -> Phrases {
		if let (Ok(index), Ok(image)) = (self.read_file(PHRINDEX), self.read_file(PHRIMAGE)) {
			return Phrases::parse_hall(index, image).unwrap_or_default();
		}
		self.read_file(PHRASES)
			.ok()
			.and_then(|buf| Phrases::parse_phrases(buf, false).ok())
			.unwrap_or_default()
	}

	/// Fill in titles from `|TTLBTREE` for topics whose header carried none.
	///
	/// The `$` footnote reaches the topic header, but a file compiled without it still
	/// lists titles in this index, and a titled topic is much easier to navigate.
	fn apply_stored_titles(&self, topics: &mut [Topic]) {
		let Ok(titles) = self.titles() else { return };
		for topic in topics.iter_mut().filter(|t| t.title.is_empty()) {
			if let Some(title) = titles.get(&topic.offset) {
				topic.title.clone_from(title);
			}
		}
	}

	/// The topic offset a link points at, or `None` if nothing in the file claims that
	/// context.
	///
	/// A link that leaves the file, runs a macro, or names a topic by number has no offset
	/// to give; match on [`Link::target`] for those. Compare the result against
	/// [`Topic::offset`] to find the topic itself.
	#[must_use]
	pub fn topic_offset(&self, link: &Link) -> Option<u32> {
		match link.target {
			Target::Context(hash) => self.contexts.get(&hash).copied(),
			_ => None,
		}
	}

	/// Every context hash the file defines, mapped to the topic offset it names.
	///
	/// [`crate::hash`] turns a context string into a key for this map.
	#[must_use]
	pub const fn contexts(&self) -> &HashMap<u32, u32> {
		&self.contexts
	}

	/// Read `|CONTEXT`, which is a B+ tree of context hashes and topic offsets.
	///
	/// A file with no `|CONTEXT`, or a damaged one, simply resolves no links: that costs
	/// the caller its cross references, not its text.
	fn read_contexts(&self) -> HashMap<u32, u32> {
		let Ok(buf) = self.read_file(CONTEXT) else { return HashMap::new() };
		let Ok(tree) = Btree::parse(buf, CONTEXT) else { return HashMap::new() };
		tree.entries(|r| Ok((r.u32()?, r.u32()?))).map(|e| e.into_iter().collect()).unwrap_or_default()
	}

	/// The topic titles stored in `|TTLBTREE`, keyed by topic offset.
	///
	/// # Errors
	///
	/// Returns [`HlpError::NotFound`] if the file has no such index, and
	/// [`HlpError::BadBtree`] if it is malformed.
	pub fn titles(&self) -> Result<HashMap<u32, String>> {
		let buf = self.read_file(TITLES)?;
		let tree = Btree::parse(buf, TITLES)?;
		let entries = tree.entries(|r| Ok((r.u32()?, text::decode(r.cstr()?))))?;
		Ok(entries.into_iter().collect())
	}
}

/// Read the internal directory, which is a B+ tree of names and file offsets.
fn read_directory(data: &[u8], offset: u32) -> Result<HashMap<String, u32>> {
	let buf = format::internal_file_at(data, offset)?;
	let tree = Btree::parse(buf, DIRECTORY)?;
	let entries = tree.entries(|r: &mut Reader<'_>| {
		let name = text::decode(r.cstr()?);
		Ok((name, r.u32()?))
	})?;
	Ok(entries.into_iter().collect())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_file_that_is_not_winhelp_is_rejected() {
		assert!(matches!(HlpFile::from_bytes(b"Not a help file at all".to_vec()), Err(HlpError::BadMagic)));
		assert!(matches!(HlpFile::from_bytes(Vec::new()), Err(HlpError::Truncated)));
	}

	// A Borland or QuickHelp file shares the extension but nothing else, so the magic
	// check is the only thing standing between them and a nonsense parse.
	#[test]
	fn a_foreign_hlp_file_is_rejected_on_its_magic() {
		let mut buf = b"\x4c\x4e\x02\x00".to_vec();
		buf.extend_from_slice(&[0u8; 64]);
		assert!(matches!(HlpFile::from_bytes(buf), Err(HlpError::BadMagic)));
	}

	#[test]
	fn a_header_with_no_directory_behind_it_is_reported() {
		let mut buf = Vec::new();
		buf.extend_from_slice(&format::MAGIC.to_le_bytes());
		buf.extend_from_slice(&0x1000u32.to_le_bytes());
		buf.extend_from_slice(&u32::MAX.to_le_bytes());
		buf.extend_from_slice(&64u32.to_le_bytes());
		buf.resize(64, 0);
		assert!(matches!(HlpFile::from_bytes(buf), Err(HlpError::BadFileHeader(_))));
	}

	// The header states the whole file's size, so a file cut short can be told apart from
	// one that is merely malformed.
	#[test]
	fn a_file_shorter_than_its_header_claims_is_truncated() {
		let mut buf = Vec::new();
		buf.extend_from_slice(&format::MAGIC.to_le_bytes());
		buf.extend_from_slice(&16u32.to_le_bytes());
		buf.extend_from_slice(&u32::MAX.to_le_bytes());
		buf.extend_from_slice(&100_000u32.to_le_bytes());
		assert!(matches!(HlpFile::from_bytes(buf), Err(HlpError::Truncated)));
	}
}
