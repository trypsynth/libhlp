//! The `|FONT` internal file: the fonts a help file uses, and the character formatting
//! each one carries.
//!
//! Topic text does not say "bold". It says "switch to font 4", and font 4's descriptor in
//! here is the bold one. So recovering emphasis means reading this table first and looking
//! each font change up in it.

use crate::{
	error::{HlpError, Result},
	format::Reader,
	text,
};

/// Bytes in the fixed part of the `|FONT` header.
const HEADER_LEN: usize = 8;

/// A `FacenamesOffset` at or above this means the newer 42 byte descriptor layout.
const NEWFONT_THRESHOLD: u16 = 12;

/// A `FacenamesOffset` at or above this means the media view descriptor layout.
const MVBFONT_THRESHOLD: u16 = 16;

/// Bytes in the WinHelp 3.1 descriptor, which packs its attributes into one byte.
const OLDFONT_LEN: usize = 11;

/// Bytes in both of the newer descriptors, which spell each attribute out.
const NEWFONT_LEN: usize = 42;

/// Windows font weights at or above this read as bold.
const BOLD_WEIGHT: i16 = 500;

/// Bits of the WinHelp 3.1 descriptor's attribute byte.
mod attrs {
	pub const BOLD: u8 = 0x01;
	pub const ITALIC: u8 = 0x02;
	pub const UNDERLINE: u8 = 0x04;
	pub const STRIKEOUT: u8 = 0x08;
	pub const DOUBLE_UNDERLINE: u8 = 0x10;
	pub const SMALL_CAPS: u8 = 0x20;
}

/// The character formatting a stretch of text is set in.
// A flag per attribute is what the file itself stores, and a caller wants to test them one
// at a time, so packing them into anything cleverer would only get unpacked again.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Format {
	/// Whether the text is bold.
	pub bold: bool,
	/// Whether the text is italic.
	pub italic: bool,
	/// Whether the text is underlined, singly or doubly.
	pub underline: bool,
	/// Whether the text is struck through.
	pub strikeout: bool,
	/// Whether the text is set in small capitals.
	pub small_caps: bool,
}

impl Format {
	/// Whether this is plain text with no emphasis at all.
	#[must_use]
	pub const fn is_plain(&self) -> bool {
		!self.bold && !self.italic && !self.underline && !self.strikeout && !self.small_caps
	}
}

/// One entry of the font table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Font {
	/// The Windows font name, as the author's machine knew it.
	pub face: String,
	/// Size in half points, so 20 is a 10 point font. Zero when the file does not say.
	pub half_points: u8,
	/// The emphasis this font applies.
	pub format: Format,
}

/// The file's font table, indexed by the numbers its font-change commands use.
#[derive(Debug, Default)]
pub struct Fonts {
	fonts: Vec<Font>,
}

impl Fonts {
	/// Parse the `|FONT` internal file.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadFont`] if the header is too short or describes a descriptor
	/// array that starts before the face names it follows.
	pub fn parse(buf: &[u8]) -> Result<Self> {
		if buf.len() < HEADER_LEN {
			return Err(HlpError::BadFont("truncated header"));
		}
		let mut r = Reader::new(buf);
		let bad = |_| HlpError::BadFont("truncated header");
		let face_count = usize::from(r.u16().map_err(bad)?);
		let font_count = usize::from(r.u16().map_err(bad)?);
		let faces_offset = r.u16().map_err(bad)?;
		let descriptors_offset = r.u16().map_err(bad)?;
		if descriptors_offset < faces_offset {
			return Err(HlpError::BadFont("descriptors start before face names"));
		}
		let faces = read_faces(buf, faces_offset, descriptors_offset, face_count);
		// Which descriptor layout a file uses is not stated anywhere. It is inferred from
		// how much room the header left itself: a header with style or character-map
		// fields pushes the face names further out, and those are the newer formats.
		let layout = if faces_offset >= MVBFONT_THRESHOLD {
			Layout::Mvb
		} else if faces_offset >= NEWFONT_THRESHOLD {
			Layout::New
		} else {
			Layout::Old
		};
		let fonts = (0..font_count)
			.map_while(|i| {
				let start = usize::from(descriptors_offset) + i * layout.len();
				buf.get(start..start + layout.len()).map(|d| layout.read(d, &faces))
			})
			.collect();
		Ok(Self { fonts })
	}

	/// The formatting font `index` applies, or plain text if the file has no such font.
	#[must_use]
	pub fn format(&self, index: u16) -> Format {
		self.fonts.get(usize::from(index)).map(|f| f.format).unwrap_or_default()
	}

	/// Every font the file declares, in the order its font-change commands index them.
	#[must_use]
	pub fn fonts(&self) -> &[Font] {
		&self.fonts
	}
}

/// Which of the three descriptor layouts a file uses.
#[derive(Debug, Clone, Copy)]
enum Layout {
	/// WinHelp 3.1: eleven bytes, attributes packed into the first.
	Old,
	/// HCW 4.00: forty-two bytes, one byte per attribute.
	New,
	/// Media view: forty-two bytes again, but the fields sit at different offsets.
	Mvb,
}

impl Layout {
	const fn len(self) -> usize {
		match self {
			Self::Old => OLDFONT_LEN,
			Self::New | Self::Mvb => NEWFONT_LEN,
		}
	}

	/// Decode one descriptor. `d` is exactly [`Self::len`] bytes.
	fn read(self, d: &[u8], faces: &[String]) -> Font {
		match self {
			Self::Old => {
				let a = d[0];
				Font {
					face: face_at(faces, u16::from_le_bytes([d[3], d[4]])),
					half_points: d[1],
					format: Format {
						bold: a & attrs::BOLD != 0,
						italic: a & attrs::ITALIC != 0,
						underline: a & (attrs::UNDERLINE | attrs::DOUBLE_UNDERLINE) != 0,
						strikeout: a & attrs::STRIKEOUT != 0,
						small_caps: a & attrs::SMALL_CAPS != 0,
					},
				}
			}
			// The two newer layouts differ only in where their fields sit, so they share
			// one decoder parameterised by the offsets.
			Self::New => decode_wide(d, faces, 1, 30, 34),
			Self::Mvb => decode_wide(d, faces, 0, 28, 32),
		}
	}
}

/// Decode a 42 byte descriptor whose face index, weight, and attribute run start at the
/// given offsets. The five attribute bytes always run italic, underline, strikeout, double
/// underline, small caps.
fn decode_wide(d: &[u8], faces: &[String], face: usize, weight: usize, flags: usize) -> Font {
	let weight = i16::from_le_bytes([d[weight], d[weight + 1]]);
	Font {
		face: face_at(faces, u16::from_le_bytes([d[face], d[face + 1]])),
		// These layouts give a height in twips rather than a point size, and a reader has
		// no use for the difference, so the size is left unset.
		half_points: 0,
		format: Format {
			// There is no bold flag: weight carries it, the way a Windows LOGFONT does.
			bold: weight > BOLD_WEIGHT,
			italic: d[flags] != 0,
			underline: d[flags + 1] != 0 || d[flags + 3] != 0,
			strikeout: d[flags + 2] != 0,
			small_caps: d[flags + 4] != 0,
		},
	}
}

/// The face name at `index`, or an empty string when the file names no such face.
fn face_at(faces: &[String], index: u16) -> String {
	faces.get(usize::from(index)).cloned().unwrap_or_default()
}

/// Read the face name array, which is a run of fixed width, possibly unterminated strings.
fn read_faces(buf: &[u8], start: u16, end: u16, count: usize) -> Vec<String> {
	if count == 0 {
		return Vec::new();
	}
	let width = usize::from(end - start) / count;
	if width == 0 {
		return Vec::new();
	}
	(0..count)
		.map_while(|i| {
			let at = usize::from(start) + i * width;
			buf.get(at..at + width).map(text::decode_cstr)
		})
		.collect()
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation)]

	use super::*;

	/// Build a `|FONT` file in the WinHelp 3.1 layout.
	fn old_font_file(faces: &[&str], descriptors: &[(u8, u8, u16)]) -> Vec<u8> {
		let width = 20usize;
		let faces_offset = HEADER_LEN;
		let descriptors_offset = faces_offset + faces.len() * width;
		let mut buf = Vec::new();
		buf.extend_from_slice(&(faces.len() as u16).to_le_bytes());
		buf.extend_from_slice(&(descriptors.len() as u16).to_le_bytes());
		buf.extend_from_slice(&(faces_offset as u16).to_le_bytes());
		buf.extend_from_slice(&(descriptors_offset as u16).to_le_bytes());
		for face in faces {
			let mut name = face.as_bytes().to_vec();
			name.resize(width, 0);
			buf.extend_from_slice(&name);
		}
		for (attributes, half_points, face) in descriptors {
			buf.push(*attributes);
			buf.push(*half_points);
			buf.push(3);
			buf.extend_from_slice(&face.to_le_bytes());
			buf.extend_from_slice(&[0; 6]);
		}
		buf
	}

	/// Build a `|FONT` file in one of the two 42 byte layouts.
	fn wide_font_file(faces_offset: u16, weight: i16, italic: bool, underline: bool) -> Vec<u8> {
		let mut buf = Vec::new();
		buf.extend_from_slice(&0u16.to_le_bytes());
		buf.extend_from_slice(&1u16.to_le_bytes());
		buf.extend_from_slice(&faces_offset.to_le_bytes());
		buf.extend_from_slice(&faces_offset.to_le_bytes());
		buf.resize(usize::from(faces_offset), 0);
		let mut d = [0u8; NEWFONT_LEN];
		let (weight_at, flags_at) = if faces_offset >= MVBFONT_THRESHOLD { (28, 32) } else { (30, 34) };
		d[weight_at..weight_at + 2].copy_from_slice(&weight.to_le_bytes());
		d[flags_at] = u8::from(italic);
		d[flags_at + 1] = u8::from(underline);
		buf.extend_from_slice(&d);
		buf
	}

	#[test]
	fn the_old_layout_unpacks_its_attribute_byte() {
		let buf = old_font_file(&["Helv", "Arial"], &[(0, 20, 0), (attrs::BOLD, 20, 0), (attrs::UNDERLINE, 24, 1)]);
		let fonts = Fonts::parse(&buf).unwrap();
		assert_eq!(fonts.fonts().len(), 3);
		assert!(fonts.format(0).is_plain());
		assert_eq!(fonts.format(1), Format { bold: true, ..Format::default() });
		assert_eq!(fonts.format(2), Format { underline: true, ..Format::default() });
		assert_eq!(fonts.fonts()[0].face, "Helv");
		assert_eq!(fonts.fonts()[2].face, "Arial");
		assert_eq!(fonts.fonts()[2].half_points, 24);
	}

	// A doubly underlined font is still underlined, and a reader has no way to show the
	// difference, so both bits fold into one flag.
	#[test]
	fn a_double_underline_reads_as_an_underline() {
		let buf = old_font_file(&["Helv"], &[(attrs::DOUBLE_UNDERLINE, 20, 0)]);
		assert!(Fonts::parse(&buf).unwrap().format(0).underline);
	}

	#[test]
	fn every_old_attribute_bit_is_decoded() {
		let all = attrs::BOLD | attrs::ITALIC | attrs::UNDERLINE | attrs::STRIKEOUT | attrs::SMALL_CAPS;
		let buf = old_font_file(&["Helv"], &[(all, 20, 0)]);
		let f = fonts_format(&buf);
		assert_eq!(f, Format { bold: true, italic: true, underline: true, strikeout: true, small_caps: true });
		assert!(!f.is_plain());
	}

	fn fonts_format(buf: &[u8]) -> Format {
		Fonts::parse(buf).unwrap().format(0)
	}

	// The newer layouts have no bold bit at all: a Windows font weight stands in for it,
	// so reading bold means comparing against the normal weight rather than testing a flag.
	#[test]
	fn the_newer_layouts_take_bold_from_the_font_weight() {
		let normal = wide_font_file(12, 400, false, false);
		assert!(!fonts_format(&normal).bold);
		let heavy = wide_font_file(12, 700, false, false);
		assert!(fonts_format(&heavy).bold);
	}

	// The media view layout holds the same fields two bytes earlier than the HCW 4.00 one.
	// Reading one with the other's offsets picks up neighbouring fields as attributes.
	#[test]
	fn the_media_view_layout_uses_its_own_offsets() {
		let mvb = wide_font_file(16, 700, true, false);
		assert_eq!(fonts_format(&mvb), Format { bold: true, italic: true, ..Format::default() });
		let new = wide_font_file(12, 700, true, false);
		assert_eq!(fonts_format(&new), Format { bold: true, italic: true, ..Format::default() });
	}

	#[test]
	fn an_unknown_font_number_reads_as_plain_text() {
		let buf = old_font_file(&["Helv"], &[(attrs::BOLD, 20, 0)]);
		let fonts = Fonts::parse(&buf).unwrap();
		assert!(fonts.format(99).is_plain());
	}

	#[test]
	fn a_file_with_no_fonts_parses_to_an_empty_table() {
		let buf = old_font_file(&[], &[]);
		let fonts = Fonts::parse(&buf).unwrap();
		assert!(fonts.fonts().is_empty());
		assert!(fonts.format(0).is_plain());
	}

	#[test]
	fn a_descriptor_array_running_past_the_file_stops_early() {
		let mut buf = old_font_file(&["Helv"], &[(attrs::BOLD, 20, 0), (attrs::ITALIC, 20, 0)]);
		buf.truncate(buf.len() - 6);
		let fonts = Fonts::parse(&buf).unwrap();
		assert_eq!(fonts.fonts().len(), 1);
		assert!(fonts.format(0).bold);
	}

	#[test]
	fn a_malformed_header_is_rejected() {
		assert!(matches!(Fonts::parse(&[]), Err(HlpError::BadFont(_))));
		let mut buf = old_font_file(&["Helv"], &[(0, 20, 0)]);
		// Put the descriptors before the face names.
		buf[6..8].copy_from_slice(&0u16.to_le_bytes());
		assert!(matches!(Fonts::parse(&buf), Err(HlpError::BadFont(_))));
	}
}
