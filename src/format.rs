//! Binary layout of the WinHelp container.
//!
//! The outer file header, the `FILEHEADER` that precedes every internal file, and the
//! bounds-checked little-endian readers the rest of the crate parses with.

use crate::error::{HlpError, Result};

/// First four bytes of every WinHelp file.
pub const MAGIC: u32 = 0x0003_5F3F;

/// Bytes in the outer file header: magic, directory offset, free list offset, file size.
pub const FILE_HEADER_LEN: usize = 16;

/// Bytes in the `FILEHEADER` that precedes each internal file's contents.
pub const INTERNAL_HEADER_LEN: usize = 9;

/// Bias subtracted from the two byte form of a compressed long.
const CLONG_NARROW_BIAS: i64 = 0x4000;

/// Bias subtracted from the four byte form of a compressed long.
const CLONG_WIDE_BIAS: i64 = 0x4000_0000;

/// The outer header, at offset 0 and the only structure at a fixed place.
#[derive(Debug, Clone, Copy)]
pub struct FileHeader {
	/// Offset of the internal directory's `FILEHEADER`.
	pub directory_start: u32,
	/// Size the header claims the whole file is.
	pub entire_file_size: u32,
}

/// A cursor over a byte slice that reports a truncated read rather than panicking.
///
/// Every multi-byte read is bounds checked, so callers can parse straight through a
/// structure and check once at the end.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
	buf: &'a [u8],
	pos: usize,
}

impl<'a> Reader<'a> {
	/// Start reading `buf` from its first byte.
	pub const fn new(buf: &'a [u8]) -> Self {
		Self { buf, pos: 0 }
	}

	/// Start reading `buf` from byte `pos`, which may be past the end.
	pub const fn at(buf: &'a [u8], pos: usize) -> Self {
		Self { buf, pos }
	}

	/// The index of the next byte to be read.
	pub const fn pos(&self) -> usize {
		self.pos
	}

	/// Whether every byte of the underlying slice has been consumed.
	pub const fn is_empty(&self) -> bool {
		self.pos >= self.buf.len()
	}

	/// How many bytes remain unread.
	pub const fn remaining(&self) -> usize {
		self.buf.len().saturating_sub(self.pos)
	}

	/// Move the cursor to `pos`, which may be past the end.
	pub const fn seek(&mut self, pos: usize) {
		self.pos = pos;
	}

	/// Advance the cursor by `n` bytes without reading them.
	pub fn skip(&mut self, n: usize) -> Result<()> {
		self.pos = self.pos.checked_add(n).ok_or(HlpError::Overflow)?;
		Ok(())
	}

	/// Read `n` bytes.
	pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
		let end = self.pos.checked_add(n).ok_or(HlpError::Overflow)?;
		let slice = self.buf.get(self.pos..end).ok_or(HlpError::Truncated)?;
		self.pos = end;
		Ok(slice)
	}

	/// Read one byte.
	pub fn u8(&mut self) -> Result<u8> {
		Ok(self.take(1)?[0])
	}

	/// Read a little-endian `u16`.
	pub fn u16(&mut self) -> Result<u16> {
		Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
	}

	/// Read a little-endian `i16`.
	pub fn i16(&mut self) -> Result<i16> {
		Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
	}

	/// Read a little-endian `u32`.
	pub fn u32(&mut self) -> Result<u32> {
		Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
	}

	/// Read a NUL-terminated byte string, consuming the terminator.
	///
	/// A string running to the end of the buffer with no terminator is returned whole,
	/// since help files in the wild do truncate the last string of an internal file.
	pub fn cstr(&mut self) -> Result<&'a [u8]> {
		let rest = self.buf.get(self.pos..).ok_or(HlpError::Truncated)?;
		let end = rest.iter().position(|&b| b == 0);
		let (bytes, consumed) = end.map_or((rest, rest.len()), |i| (&rest[..i], i + 1));
		self.pos += consumed;
		Ok(bytes)
	}

	/// Read a "compressed unsigned short": one byte, or two when the low bit is set.
	///
	/// Even values halve to themselves; odd values halve and take 128 times the next byte.
	pub fn cushort(&mut self) -> Result<u16> {
		let b = u16::from(self.u8()?);
		if b.is_multiple_of(2) {
			return Ok(b / 2);
		}
		let hi = u16::from(self.u8()?);
		Ok((b / 2).wrapping_add(hi.wrapping_mul(128)))
	}

	/// Read a "compressed signed short", which is [`Self::cushort`] biased by -64 or -16384.
	pub fn cshort(&mut self) -> Result<i16> {
		let b = i32::from(self.u8()?);
		if b % 2 == 0 {
			#[allow(clippy::cast_possible_truncation)]
			return Ok((b / 2 - 64) as i16);
		}
		let hi = i32::from(self.u8()?);
		#[allow(clippy::cast_possible_truncation)]
		Ok((b / 2 + 128 * hi - 16384) as i16)
	}

	/// Read a "compressed long": two bytes, or four when the low bit is set, each biased
	/// so the encoded range straddles zero.
	///
	/// helpfile.txt gives the four byte bias as 67108864. The bias the compilers actually
	/// use, and that helpdeco reads, is [`CLONG_WIDE_BIAS`], the value that makes the two
	/// widths agree on where zero sits.
	pub fn clong(&mut self) -> Result<i32> {
		let w = i64::from(self.u16()?);
		if w % 2 == 0 {
			#[allow(clippy::cast_possible_truncation)]
			return Ok((w / 2 - CLONG_NARROW_BIAS) as i32);
		}
		let hi = i64::from(self.u16()?);
		let raw = w + 0x1_0000 * hi;
		#[allow(clippy::cast_possible_truncation)]
		Ok((raw / 2 - CLONG_WIDE_BIAS) as i32)
	}
}

/// Parse the outer file header from the first bytes of a help file.
///
/// # Errors
///
/// Returns [`HlpError::BadMagic`] if the magic number is wrong, or [`HlpError::Truncated`]
/// if fewer than [`FILE_HEADER_LEN`] bytes are available.
pub fn parse_file_header(buf: &[u8]) -> Result<FileHeader> {
	if buf.len() < FILE_HEADER_LEN {
		return Err(HlpError::Truncated);
	}
	let mut r = Reader::new(buf);
	if r.u32()? != MAGIC {
		return Err(HlpError::BadMagic);
	}
	let directory_start = r.u32()?;
	// FirstFreeBlock. Free space is of no interest to a reader.
	r.skip(4)?;
	let entire_file_size = r.u32()?;
	Ok(FileHeader { directory_start, entire_file_size })
}

/// The bytes of the internal file whose `FILEHEADER` sits at `offset` in `buf`.
///
/// # Errors
///
/// Returns [`HlpError::BadFileHeader`] if the header runs past the end of the file or
/// claims more content than the file holds.
pub fn internal_file_at(buf: &[u8], offset: u32) -> Result<&[u8]> {
	let offset = offset as usize;
	let mut r = Reader::at(buf, offset);
	let bad = || HlpError::BadFileHeader(offset as u64);
	// ReservedSpace covers the header and any free tail, so only UsedSpace bounds the
	// content. FileFlags, the last header byte, says nothing a reader needs.
	r.skip(4).map_err(|_| bad())?;
	let used = r.u32().map_err(|_| bad())?;
	r.skip(INTERNAL_HEADER_LEN - 8).map_err(|_| bad())?;
	let used = usize::try_from(used).map_err(|_| HlpError::Overflow)?;
	r.take(used).map_err(|_| bad())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn file_header_reads_offsets() {
		let mut buf = Vec::new();
		buf.extend_from_slice(&MAGIC.to_le_bytes());
		buf.extend_from_slice(&0x1234u32.to_le_bytes());
		buf.extend_from_slice(&u32::MAX.to_le_bytes());
		buf.extend_from_slice(&0x9999u32.to_le_bytes());
		let header = parse_file_header(&buf).unwrap();
		assert_eq!(header.directory_start, 0x1234);
		assert_eq!(header.entire_file_size, 0x9999);
	}

	#[test]
	fn file_header_rejects_foreign_files() {
		let buf = [0u8; FILE_HEADER_LEN];
		assert!(matches!(parse_file_header(&buf), Err(HlpError::BadMagic)));
		let short = MAGIC.to_le_bytes();
		assert!(matches!(parse_file_header(&short), Err(HlpError::Truncated)));
		assert!(matches!(parse_file_header(&[]), Err(HlpError::Truncated)));
	}

	#[test]
	fn internal_file_returns_only_used_space() {
		let mut buf = vec![0u8; 4];
		// ReservedSpace 32, UsedSpace 5, FileFlags 4, then content and slack.
		buf.extend_from_slice(&32u32.to_le_bytes());
		buf.extend_from_slice(&5u32.to_le_bytes());
		buf.push(4);
		buf.extend_from_slice(b"hello");
		buf.extend_from_slice(&[0xff; 18]);
		assert_eq!(internal_file_at(&buf, 4).unwrap(), b"hello");
	}

	#[test]
	fn internal_file_rejects_content_past_end() {
		let mut buf = vec![0u8; 4];
		buf.extend_from_slice(&32u32.to_le_bytes());
		buf.extend_from_slice(&500u32.to_le_bytes());
		buf.push(4);
		assert!(matches!(internal_file_at(&buf, 4), Err(HlpError::BadFileHeader(4))));
	}

	#[test]
	fn cstr_stops_at_the_terminator() {
		let mut r = Reader::new(b"one\0two\0");
		assert_eq!(r.cstr().unwrap(), b"one");
		assert_eq!(r.cstr().unwrap(), b"two");
		assert!(r.is_empty());
	}

	#[test]
	fn cstr_accepts_an_unterminated_tail() {
		let mut r = Reader::new(b"tail");
		assert_eq!(r.cstr().unwrap(), b"tail");
		assert!(r.is_empty());
	}

	// The compressed integer encodings all key off the low bit of the first byte, so each
	// needs both widths exercised or a one-byte reader silently truncates every long value.
	#[test]
	fn compressed_unsigned_short_uses_one_or_two_bytes() {
		assert_eq!(Reader::new(&[0x08]).cushort().unwrap(), 4);
		assert_eq!(Reader::new(&[0x09, 0x02]).cushort().unwrap(), 4 + 256);
	}

	#[test]
	fn compressed_signed_short_is_biased() {
		assert_eq!(Reader::new(&[0x80]).cshort().unwrap(), 0);
		assert_eq!(Reader::new(&[0x00]).cshort().unwrap(), -64);
		assert_eq!(Reader::new(&[0x01, 0x80]).cshort().unwrap(), 0);
	}

	// The two widths of a compressed long use different biases, and the four byte one is
	// the field helpfile.txt gets wrong, so both need pinning down.
	#[test]
	fn compressed_long_uses_two_or_four_bytes() {
		assert_eq!(Reader::new(&[0x00, 0x80]).clong().unwrap(), 0);
		assert_eq!(Reader::new(&[0x00, 0x00]).clong().unwrap(), -0x4000);
		assert_eq!(Reader::new(&[0x01, 0x00, 0x00, 0x80]).clong().unwrap(), 0);
		assert_eq!(Reader::new(&[0x01, 0x00, 0x00, 0x00]).clong().unwrap(), -0x4000_0000);
	}

	#[test]
	fn reads_past_the_end_report_truncation() {
		let mut r = Reader::new(&[1, 2, 3]);
		assert!(matches!(r.u32(), Err(HlpError::Truncated)));
	}
}
