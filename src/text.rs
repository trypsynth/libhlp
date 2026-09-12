//! Turning the byte strings in a help file into Rust strings.
//!
//! WinHelp predates Unicode. Its text is in whatever single-byte Windows code page the
//! author's machine used, named only loosely by the `|SYSTEM` charset record. This crate
//! decodes as Windows-1252, which covers Western European files, and exposes the charset
//! so a caller that needs another code page can re-decode the raw internal file itself.

/// The 32 characters where Windows-1252 differs from Latin-1, covering bytes 0x80 to 0x9F.
const CP1252_HIGH: [char; 32] = [
	'\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{02C6}',
	'\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}',
	'\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}',
	'\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
];

/// Decode `bytes` as Windows-1252.
pub fn decode(bytes: &[u8]) -> String {
	bytes
		.iter()
		.map(|&b| match b {
			0x80..=0x9f => CP1252_HIGH[usize::from(b) - 0x80],
			_ => char::from(b),
		})
		.collect()
}

/// Decode `bytes` as Windows-1252, stopping at the first NUL.
///
/// Some records store a string padded out to a fixed size, so the declared length is not
/// the string's length.
pub fn decode_cstr(bytes: &[u8]) -> String {
	let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
	decode(&bytes[..end])
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ascii_passes_through() {
		assert_eq!(decode(b"Hello, world."), "Hello, world.");
	}

	// Latin-1 and Windows-1252 agree above 0x9F, so accented text needs no special case.
	#[test]
	fn high_latin1_bytes_map_to_their_own_code_points() {
		assert_eq!(decode(&[0xe9, 0xfc, 0xdf]), "éüß");
	}

	// The 0x80 to 0x9F range is the whole reason this is not just a Latin-1 cast: WinHelp
	// files are full of smart quotes and dashes that Latin-1 would turn into control codes.
	#[test]
	fn the_windows_range_maps_to_punctuation() {
		assert_eq!(decode(&[0x93, 0x94, 0x97, 0x85]), "\u{201C}\u{201D}\u{2014}\u{2026}");
		assert_eq!(decode(&[0x80]), "\u{20AC}");
	}

	#[test]
	fn unassigned_bytes_become_the_replacement_character() {
		assert_eq!(decode(&[0x81]), "\u{FFFD}");
	}

	#[test]
	fn a_c_string_stops_at_its_terminator() {
		assert_eq!(decode_cstr(b"Title\0padding\0"), "Title");
		assert_eq!(decode_cstr(b"No terminator"), "No terminator");
		assert_eq!(decode_cstr(b""), "");
	}
}
