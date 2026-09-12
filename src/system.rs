//! The `|SYSTEM` internal file: format version, compression scheme, and file metadata.
//!
//! Everything else in the file is decoded according to what this says, so it is read
//! first and its answers are carried around in [`System`].

use crate::{
	error::{HlpError, Result},
	format::Reader,
	text,
};

/// First two bytes of the `SYSTEMHEADER`.
const MAGIC: u16 = 0x036C;

/// Format versions below this are WinHelp 3.0, which lays out the whole file differently.
const FIRST_31_VERSION: u16 = 16;

/// `Flags` value meaning LZ77 compressed topic blocks of 4k.
const FLAGS_LZ77_4K: u16 = 4;

/// `Flags` value meaning LZ77 compressed topic blocks of 2k.
const FLAGS_LZ77_2K: u16 = 8;

/// Topic block size used by WinHelp 3.0 and by 3.1 files with [`FLAGS_LZ77_2K`].
const BLOCK_2K: usize = 2048;

/// Topic block size used by every other WinHelp 3.1 file.
const BLOCK_4K: usize = 4096;

/// Size a compressed topic block expands into, and the stride of the topic address space.
const DECOMPRESS_16K: usize = 0x4000;

/// A `SYSTEMREC` holding the help file title.
const REC_TITLE: u16 = 1;

/// A `SYSTEMREC` holding the copyright notice.
const REC_COPYRIGHT: u16 = 2;

/// A `SYSTEMREC` holding the topic offset of the contents topic.
const REC_CONTENTS: u16 = 3;

/// A `SYSTEMREC` holding the Windows character set the file's text is in.
const REC_CHARSET: u16 = 11;

/// What `|SYSTEM` says about the file.
#[derive(Debug, Clone)]
pub struct System {
	/// Format version: 15 for `HC30`, 21 for `HC31`, 27 for media view, 33 for HCW 4.00.
	pub version: u16,
	/// Title from the help project file, if it set one.
	pub title: Option<String>,
	/// Copyright notice shown in the About box, if the project set one.
	pub copyright: Option<String>,
	/// Topic offset of the topic WinHelp opens on.
	pub contents: Option<u32>,
	/// Windows character set the file's text is encoded in, if it says.
	pub charset: Option<u16>,
	/// Bytes each topic block occupies in `|TOPIC`.
	pub block_size: usize,
	/// Bytes a topic block expands to, which is also the stride of a topic position.
	pub decompress_size: usize,
	/// Whether topic blocks are LZ77 compressed.
	pub compressed: bool,
}

impl System {
	/// Parse the `|SYSTEM` internal file.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadSystem`] if the header is missing or has the wrong magic, and
	/// [`HlpError::UnsupportedVersion`] for a WinHelp 3.0 file.
	pub fn parse(buf: &[u8]) -> Result<Self> {
		let mut r = Reader::new(buf);
		if r.u16().map_err(|_| HlpError::BadSystem("truncated header"))? != MAGIC {
			return Err(HlpError::BadSystem("bad SYSTEMHEADER magic"));
		}
		let version = r.u16().map_err(|_| HlpError::BadSystem("truncated header"))?;
		// Major, then GenDate. Neither changes how anything is read.
		r.skip(2 + 4).map_err(|_| HlpError::BadSystem("truncated header"))?;
		let flags = r.u16().map_err(|_| HlpError::BadSystem("truncated header"))?;
		if version < FIRST_31_VERSION {
			return Err(HlpError::UnsupportedVersion(version));
		}
		let compressed = flags == FLAGS_LZ77_4K || flags == FLAGS_LZ77_2K;
		let block_size = if flags == FLAGS_LZ77_2K { BLOCK_2K } else { BLOCK_4K };
		let mut system = Self {
			version,
			title: None,
			copyright: None,
			contents: None,
			charset: None,
			block_size,
			// Topic positions are strided by this whether or not blocks are compressed, so
			// it is not simply the size a block happens to expand to.
			decompress_size: DECOMPRESS_16K,
			compressed,
		};
		system.read_records(&mut r);
		Ok(system)
	}

	/// Read the `SYSTEMREC` list, keeping the records this crate has a use for.
	///
	/// A record that runs off the end ends the list: the tail of `|SYSTEM` is padding in
	/// some files, and a short read there should not lose the records already gathered.
	fn read_records(&mut self, r: &mut Reader<'_>) {
		while !r.is_empty() {
			let (Ok(kind), Ok(size)) = (r.u16(), r.u16()) else { break };
			let Ok(data) = r.take(usize::from(size)) else { break };
			match kind {
				REC_TITLE => self.title = Some(text::decode_cstr(data)),
				REC_COPYRIGHT => self.copyright = Some(text::decode_cstr(data)),
				REC_CONTENTS => {
					self.contents = data.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
				}
				REC_CHARSET => {
					self.charset = data.get(..2).map(|b| u16::from_le_bytes(b.try_into().unwrap()));
				}
				_ => {}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation)]

	use super::*;

	fn header(version: u16, flags: u16) -> Vec<u8> {
		let mut buf = Vec::new();
		buf.extend_from_slice(&MAGIC.to_le_bytes());
		buf.extend_from_slice(&version.to_le_bytes());
		buf.extend_from_slice(&1u16.to_le_bytes());
		buf.extend_from_slice(&0u32.to_le_bytes());
		buf.extend_from_slice(&flags.to_le_bytes());
		buf
	}

	fn record(buf: &mut Vec<u8>, kind: u16, data: &[u8]) {
		buf.extend_from_slice(&kind.to_le_bytes());
		buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
		buf.extend_from_slice(data);
	}

	// The three flag values pick different block geometry, and getting one wrong misreads
	// every topic position in the file.
	#[test]
	fn flags_select_the_block_geometry() {
		let plain = System::parse(&header(21, 0)).unwrap();
		assert!(!plain.compressed);
		assert_eq!(plain.block_size, BLOCK_4K);
		assert_eq!(plain.decompress_size, DECOMPRESS_16K);
		let big = System::parse(&header(21, FLAGS_LZ77_4K)).unwrap();
		assert!(big.compressed);
		assert_eq!(big.block_size, BLOCK_4K);
		let small = System::parse(&header(21, FLAGS_LZ77_2K)).unwrap();
		assert!(small.compressed);
		assert_eq!(small.block_size, BLOCK_2K);
	}

	#[test]
	fn records_are_picked_out_by_type() {
		let mut buf = header(33, 4);
		record(&mut buf, REC_TITLE, b"My Help\0");
		record(&mut buf, 5, &[0xde, 0xad]);
		record(&mut buf, REC_COPYRIGHT, b"(c) 1995\0");
		record(&mut buf, REC_CONTENTS, &0x1234u32.to_le_bytes());
		record(&mut buf, REC_CHARSET, &0u16.to_le_bytes());
		let system = System::parse(&buf).unwrap();
		assert_eq!(system.title.as_deref(), Some("My Help"));
		assert_eq!(system.copyright.as_deref(), Some("(c) 1995"));
		assert_eq!(system.contents, Some(0x1234));
		assert_eq!(system.charset, Some(0));
	}

	// Padding or a truncated record at the tail should not discard the records already read.
	#[test]
	fn a_short_record_ends_the_list_without_losing_earlier_ones() {
		let mut buf = header(21, 0);
		record(&mut buf, REC_TITLE, b"Kept\0");
		buf.extend_from_slice(&REC_COPYRIGHT.to_le_bytes());
		buf.extend_from_slice(&99u16.to_le_bytes());
		buf.extend_from_slice(b"short");
		let system = System::parse(&buf).unwrap();
		assert_eq!(system.title.as_deref(), Some("Kept"));
		assert!(system.copyright.is_none());
	}

	#[test]
	fn winhelp_30_is_reported_as_unsupported() {
		assert!(matches!(System::parse(&header(15, 0)), Err(HlpError::UnsupportedVersion(15))));
	}

	#[test]
	fn a_foreign_system_file_is_rejected() {
		let mut buf = header(21, 0);
		buf[0] = 0;
		assert!(matches!(System::parse(&buf), Err(HlpError::BadSystem(_))));
		assert!(matches!(System::parse(&[]), Err(HlpError::BadSystem(_))));
	}
}
