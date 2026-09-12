//! The phrase table topic text is compressed against.
//!
//! A help file replaces its most repeated word sequences with short codes, and keeps the
//! sequences themselves in one of two places. WinHelp 3.1 stores a `|Phrases` file. HCW
//! 4.00 may instead use "Hall" compression, splitting the lengths into `|PhrIndex` and the
//! characters into `|PhrImage`. Both end up as the same lookup table.

use crate::{
	error::{HlpError, Result},
	format::Reader,
	lz77,
};

/// Marks the MVB layout of `|Phrases`, which prefixes the real count and adds a 30 byte gap.
const MVB_PREFIX: u16 = 0x0800;

/// Follows the phrase count in every `|Phrases` layout.
const PHRASES_MAGIC: u16 = 0x0100;

/// Bytes of unused space between the header and the offset array in the MVB layout.
const MVB_GAP: usize = 30;

/// Bytes in the `|PhrIndex` header, after which the packed lengths start.
const PHRINDEX_HEADER_LEN: usize = 28;

/// Phrase codes below this are literal bytes in an old-style compressed stream.
const FIRST_PHRASE_CODE: u8 = 1;

/// Phrase codes at or above this are literal bytes in an old-style compressed stream.
const LAST_PHRASE_CODE: u8 = 16;

/// The phrase table, plus which of the two substitution schemes the file uses.
#[derive(Debug, Default)]
pub struct Phrases {
	/// Concatenated phrase text, indexed through `offsets`.
	text: Vec<u8>,
	/// One more entry than there are phrases, so a phrase is `offsets[n]..offsets[n + 1]`.
	offsets: Vec<u32>,
	hall: bool,
}

impl Phrases {
	/// The bytes of phrase `n`, or an empty slice if the file names one that does not exist.
	fn phrase(&self, n: usize) -> &[u8] {
		let (Some(&start), Some(&end)) = (self.offsets.get(n), self.offsets.get(n + 1)) else {
			return &[];
		};
		self.text.get(start as usize..end as usize).unwrap_or_default()
	}

	/// Parse the old-style `|Phrases` file.
	///
	/// `uncompressed` selects the WinHelp 3.0 layout, whose phrase text is stored as is
	/// rather than LZ77 compressed.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadPhrases`] if the header does not match either known layout.
	pub fn parse_phrases(buf: &[u8], uncompressed: bool) -> Result<Self> {
		let mut r = Reader::new(buf);
		let first = r.u16().map_err(|_| HlpError::BadPhrases("truncated header"))?;
		// The MVB layout puts a marker where the count belongs and the real count after it.
		let mvb = first == MVB_PREFIX;
		let count = if mvb { r.u16().map_err(|_| HlpError::BadPhrases("truncated header"))? } else { first };
		if r.u16().map_err(|_| HlpError::BadPhrases("truncated header"))? != PHRASES_MAGIC {
			return Err(HlpError::BadPhrases("unknown |Phrases layout"));
		}
		if count == 0 {
			return Ok(Self::default());
		}
		let count = usize::from(count);
		// WinHelp 3.0 has no decompressed-size field: the text is stored as it stands, so
		// its length is whatever the file has left after the offset array.
		let decompressed =
			if uncompressed { 0 } else { r.u32().map_err(|_| HlpError::BadPhrases("truncated header"))? as usize };
		if mvb {
			r.skip(MVB_GAP)?;
		}
		// Stored offsets count from the start of the file, so rebase them onto the text.
		let base = u32::try_from((count + 1) * 2).map_err(|_| HlpError::Overflow)?;
		let mut offsets = Vec::with_capacity(count + 1);
		for _ in 0..=count {
			let raw = u32::from(r.u16().map_err(|_| HlpError::BadPhrases("truncated offset array"))?);
			offsets.push(raw.saturating_sub(base));
		}
		let rest = r.take(r.remaining())?;
		let text = if uncompressed { rest.to_vec() } else { lz77::decompress(rest, decompressed) };
		Ok(Self { text, offsets, hall: false })
	}

	/// Parse the Hall pair: packed phrase lengths in `index`, phrase text in `image`.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadPhrases`] if `|PhrIndex` is too short to hold its header.
	pub fn parse_hall(index: &[u8], image: &[u8]) -> Result<Self> {
		let mut r = Reader::new(index);
		// always4A01, then the counts. compressedsize and always0 are not needed.
		r.skip(4).map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))?;
		let entries = r.u32().map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))?;
		r.skip(4).map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))?;
		let image_size = r.u32().map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))? as usize;
		let image_compressed_size = r.u32().map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))? as usize;
		r.skip(4).map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))?;
		let bits = u32::from(r.u16().map_err(|_| HlpError::BadPhrases("truncated |PhrIndex header"))? & 0xf);
		let text =
			if image_size == image_compressed_size { image.to_vec() } else { lz77::decompress(image, image_size) };
		let entries = entries.min(u32::try_from(image_size.max(1)).unwrap_or(u32::MAX)) as usize;
		let mut reader = BitReader::new(index, PHRINDEX_HEADER_LEN);
		let mut offsets = Vec::with_capacity(entries + 1);
		let mut offset = 0u32;
		offsets.push(0);
		for _ in 0..entries {
			// A unary run in steps of 2^bits, then the low bits of the remainder. Together
			// they give this phrase's length in bytes.
			let mut n = 1u32;
			while reader.bit() {
				n = n.saturating_add(1 << bits);
			}
			// The low bit of the remainder is always present; the rest exist only if the
			// step size is wide enough to need them.
			if reader.bit() {
				n += 1;
			}
			for shift in 1..bits.min(5) {
				if reader.bit() {
					n += 1 << shift;
				}
			}
			offset = offset.saturating_add(n);
			offsets.push(offset);
		}
		Ok(Self { text, offsets, hall: true })
	}

	/// Expand one `LinkData2` stream, substituting phrases for their codes.
	pub fn expand(&self, input: &[u8], expected: usize) -> Vec<u8> {
		let mut out = Vec::with_capacity(expected);
		if self.hall {
			self.expand_hall(input, &mut out);
		} else {
			self.expand_old(input, &mut out);
		}
		out
	}

	/// The WinHelp 3.1 scheme: a byte in 1..16 opens a two byte phrase code.
	fn expand_old(&self, input: &[u8], out: &mut Vec<u8>) {
		let mut i = 0;
		while i < input.len() {
			let ch = input[i];
			i += 1;
			if !(FIRST_PHRASE_CODE..LAST_PHRASE_CODE).contains(&ch) {
				out.push(ch);
				continue;
			}
			let Some(&next) = input.get(i) else { break };
			i += 1;
			let code = 256 * (usize::from(ch) - 1) + usize::from(next);
			out.extend_from_slice(self.phrase(code / 2));
			// An odd code means the phrase was stored without the space that followed it.
			if code % 2 == 1 {
				out.push(b' ');
			}
		}
	}

	/// The HCW 4.00 scheme, where the low bits of each byte pick between four encodings.
	fn expand_hall(&self, input: &[u8], out: &mut Vec<u8>) {
		let mut i = 0;
		while i < input.len() {
			let ch = usize::from(input[i]);
			i += 1;
			if ch & 1 == 0 {
				out.extend_from_slice(self.phrase(ch / 2));
			} else if ch & 3 == 1 {
				let Some(&next) = input.get(i) else { break };
				i += 1;
				out.extend_from_slice(self.phrase(128 + (ch / 4) * 256 + usize::from(next)));
			} else if ch & 7 == 3 {
				// The count lives in the bits above the tag, and one is always implied: a
				// run of nothing would be a wasted byte, so the encoding spends no value
				// on it.
				let take = (ch / 8 + 1).min(input.len().saturating_sub(i));
				out.extend_from_slice(&input[i..i + take]);
				i += take;
			} else if ch & 0xf == 7 {
				out.extend(std::iter::repeat_n(b' ', ch / 16 + 1));
			} else {
				out.extend(std::iter::repeat_n(0, ch / 16 + 1));
			}
		}
	}
}

/// Reads single bits out of a little-endian `u32` array, lowest bit of each word first.
///
/// `|PhrIndex` packs its lengths this way, which is why its bit stream is word aligned
/// rather than byte aligned.
struct BitReader<'a> {
	buf: &'a [u8],
	pos: usize,
	word: u32,
	mask: u32,
}

impl<'a> BitReader<'a> {
	const fn new(buf: &'a [u8], start: usize) -> Self {
		Self { buf, pos: start, word: 0, mask: 0 }
	}

	fn bit(&mut self) -> bool {
		self.mask <<= 1;
		if self.mask == 0 {
			let word = self.buf.get(self.pos..self.pos + 4);
			// Running off the end reads as zero bits, which ends any unary run in progress
			// rather than looping forever on a truncated index.
			self.word = word.map_or(0, |w| u32::from_le_bytes(w.try_into().unwrap()));
			self.pos += 4;
			self.mask = 1;
		}
		self.word & self.mask != 0
	}
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation)]

	use super::*;

	/// Build an old-style `|Phrases` file holding `words`, stored uncompressed.
	fn phrases_file(words: &[&str]) -> Vec<u8> {
		let mut buf = Vec::new();
		buf.extend_from_slice(&(words.len() as u16).to_le_bytes());
		buf.extend_from_slice(&PHRASES_MAGIC.to_le_bytes());
		let base = (words.len() + 1) * 2;
		let mut offset = base;
		for word in words {
			buf.extend_from_slice(&(offset as u16).to_le_bytes());
			offset += word.len();
		}
		buf.extend_from_slice(&(offset as u16).to_le_bytes());
		for word in words {
			buf.extend_from_slice(word.as_bytes());
		}
		buf
	}

	/// Encode an old-style phrase reference to `n`, with `space` appending a trailing blank.
	fn code(n: usize, space: bool) -> [u8; 2] {
		let raw = n * 2 + usize::from(space);
		[(raw / 256 + 1) as u8, (raw % 256) as u8]
	}

	#[test]
	fn phrases_file_round_trips() {
		let p = Phrases::parse_phrases(&phrases_file(&["hello", "world"]), true).unwrap();
		assert_eq!(p.phrase(0), b"hello");
		assert_eq!(p.phrase(1), b"world");
	}

	#[test]
	fn a_file_with_no_phrases_is_empty() {
		let p = Phrases::parse_phrases(&phrases_file(&[]), true).unwrap();
		assert_eq!(p.phrase(0), b"");
	}

	#[test]
	fn old_style_substitutes_codes_and_passes_literals() {
		let p = Phrases::parse_phrases(&phrases_file(&["hello", "world"]), true).unwrap();
		let mut stream = vec![b'>'];
		stream.extend_from_slice(&code(0, true));
		stream.extend_from_slice(&code(1, false));
		stream.push(b'!');
		assert_eq!(p.expand(&stream, 32), b">hello world!");
	}

	// Byte 0 and bytes from 16 up are literals; only 1..15 open a phrase code, so a stream
	// of ordinary text has to survive untouched.
	#[test]
	fn old_style_leaves_plain_text_alone() {
		let p = Phrases::parse_phrases(&phrases_file(&["x"]), true).unwrap();
		assert_eq!(p.expand(b"Plain text.\0", 32), b"Plain text.\0");
	}

	#[test]
	fn old_style_stops_on_a_truncated_code() {
		let p = Phrases::parse_phrases(&phrases_file(&["hello"]), true).unwrap();
		assert_eq!(p.expand(&[b'a', 1], 32), b"a");
	}

	// A code naming a phrase past the end of the table would otherwise index out of bounds.
	#[test]
	fn a_code_past_the_table_expands_to_nothing() {
		let p = Phrases::parse_phrases(&phrases_file(&["hello"]), true).unwrap();
		assert_eq!(p.expand(&code(99, false), 32), b"");
	}

	#[test]
	fn hall_reads_lengths_from_the_bit_stream() {
		// bits = 1, so each phrase length is a unary run in twos plus one low bit.
		let mut index = vec![0u8; PHRINDEX_HEADER_LEN];
		index[4..8].copy_from_slice(&2u32.to_le_bytes());
		index[12..16].copy_from_slice(&6u32.to_le_bytes());
		index[16..20].copy_from_slice(&6u32.to_le_bytes());
		index[24..26].copy_from_slice(&1u16.to_le_bytes());
		// Bits are read from the lowest up. Phrase 0: stop bit clear then low bit set
		// gives length 2. Phrase 1: one unary step, stop, low bit set gives length 4.
		let stream = 0b1_0110u32;
		index.extend_from_slice(&stream.to_le_bytes());
		let p = Phrases::parse_hall(&index, b"abcdef").unwrap();
		assert_eq!(p.phrase(0), b"ab");
		assert_eq!(p.phrase(1), b"cdef");
	}

	#[test]
	fn hall_expands_each_of_its_four_encodings() {
		let mut index = vec![0u8; PHRINDEX_HEADER_LEN];
		index[4..8].copy_from_slice(&1u32.to_le_bytes());
		index[12..16].copy_from_slice(&2u32.to_le_bytes());
		index[16..20].copy_from_slice(&2u32.to_le_bytes());
		index[24..26].copy_from_slice(&1u16.to_le_bytes());
		index.extend_from_slice(&0b10u32.to_le_bytes());
		let p = Phrases::parse_hall(&index, b"hi").unwrap();
		// 0 is phrase 0; 0x03 copies one literal; 0x07 emits one space; 0x0f emits one NUL.
		assert_eq!(p.expand(&[0, 0x03, b'!', 0x07, 0x0f], 32), b"hi! \0");
	}

	// Every run encoding implies one item beyond the count it stores, so a tag byte with a
	// zero count still produces something. Getting this wrong drops a character from every
	// run in the file.
	#[test]
	fn hall_run_counts_are_one_more_than_they_store() {
		let mut index = vec![0u8; PHRINDEX_HEADER_LEN];
		index[24..26].copy_from_slice(&1u16.to_le_bytes());
		let p = Phrases::parse_hall(&index, b"").unwrap();
		assert_eq!(p.expand(&[0x13, b'a', b'b', b'c'], 8), b"abc");
		assert_eq!(p.expand(&[0x17], 8), b"  ");
		assert_eq!(p.expand(&[0x2f], 8), b"\0\0\0");
	}

	// The unary run reads until a clear bit, so an index that ends mid-run must stop rather
	// than spin.
	#[test]
	fn a_truncated_hall_index_terminates() {
		let mut index = vec![0u8; PHRINDEX_HEADER_LEN];
		index[4..8].copy_from_slice(&4u32.to_le_bytes());
		index[12..16].copy_from_slice(&8u32.to_le_bytes());
		index[16..20].copy_from_slice(&8u32.to_le_bytes());
		index[24..26].copy_from_slice(&1u16.to_le_bytes());
		index.extend_from_slice(&u32::MAX.to_le_bytes());
		// The lengths it reads are nonsense, but it stops and still produces a table.
		let p = Phrases::parse_hall(&index, b"abcdefgh").unwrap();
		assert_eq!(p.offsets.len(), 5);
	}
}
