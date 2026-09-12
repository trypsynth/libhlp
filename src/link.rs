//! Where a hotspot in a topic leads, and how a help file names its topics.
//!
//! WinHelp does not store link targets as topic offsets. It stores the hash of the
//! target's context string, the name the author wrote in a `#` footnote, and keeps a
//! `|CONTEXT` index mapping those hashes to offsets. [`HlpFile::topic_offset`] does that
//! lookup, and [`hash`] computes the same value from a context string so a caller can find
//! a topic it knows the name of.
//!
//! [`HlpFile::topic_offset`]: crate::HlpFile::topic_offset

/// Maps each byte of a context string to the value the hash folds in.
///
/// The table is what makes the hash case insensitive: it folds `A` to `Z` onto the same
/// values as `a` to `z`, so `SaveFile` and `savefile` name the same topic.
const HASH_TABLE: [u8; 256] = [
	0x00, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE0, 0xE1, 0xE2,
	0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF, 0xF0, 0x0B, 0xF2, 0xF3, 0xF4, 0xF5,
	0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0x0C, 0xFF, 0x0A, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
	0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
	0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x0B, 0x0C, 0x0D, 0x0E,
	0x0D, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21,
	0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x50, 0x51, 0x52, 0x53, 0x54,
	0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67,
	0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
	0x7B, 0x7C, 0x7D, 0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x0B, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D,
	0x8E, 0x8F, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F, 0xA0,
	0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB0, 0xB1, 0xB2, 0xB3,
	0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6,
	0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
];

/// The hash of a context string, as `|CONTEXT` keys its entries.
///
/// Matching is case insensitive, so `SaveFile` and `savefile` hash alike. An empty string
/// hashes to 1 rather than 0, which is what WinHelp itself does.
///
/// ```
/// # use libhlp::hash;
/// assert_eq!(hash("Topic"), hash("TOPIC"));
/// assert_eq!(hash(""), 1);
/// ```
#[must_use]
pub fn hash(context: &str) -> u32 {
	if context.is_empty() {
		return 1;
	}
	// The table entries are signed and the reference implementation folds them into a
	// signed accumulator, so an entry at or above 0x80 subtracts rather than adds.
	context.bytes().fold(0u32, |acc, b| {
		let term = i32::from(i8::from_ne_bytes([HASH_TABLE[usize::from(b)]]));
		acc.wrapping_mul(43).wrapping_add_signed(term)
	})
}

/// What a hotspot points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
	/// A topic in this file, named by the hash of its context string.
	///
	/// Turn it into a topic offset with [`HlpFile::topic_offset`].
	///
	/// [`HlpFile::topic_offset`]: crate::HlpFile::topic_offset
	Context(u32),
	/// A topic in this file, named by its topic number.
	///
	/// Only WinHelp 3.0 style records name targets this way; compare against
	/// [`Topic::number`](crate::Topic::number).
	Number(u32),
	/// A topic in a different help file, named by context hash within that file.
	External {
		/// Name of the help file to open, as the author wrote it.
		file: String,
		/// Hash of the target's context string inside that file.
		context: u32,
	},
	/// Not a jump: runs a WinHelp macro.
	Macro(String),
}

/// One hotspot: where it goes, and whether WinHelp would show it in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
	/// What the hotspot points at.
	pub target: Target,
	/// Whether the target opens as a pop-up rather than replacing the current topic.
	///
	/// Pop-ups are how help files do glossary definitions and footnotes.
	pub popup: bool,
}

impl Link {
	/// A hotspot that replaces the current topic.
	#[must_use]
	pub const fn jump(target: Target) -> Self {
		Self { target, popup: false }
	}

	/// A hotspot that opens its target in place.
	#[must_use]
	pub const fn popup(target: Target) -> Self {
		Self { target, popup: true }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// The whole point of the folding table is that context strings match without regard to
	// case, so a link written in one case reaches a topic declared in another.
	#[test]
	fn hashing_ignores_case() {
		assert_eq!(hash("SaveFile"), hash("savefile"));
		assert_eq!(hash("SaveFile"), hash("SAVEFILE"));
	}

	#[test]
	fn an_empty_context_hashes_to_one() {
		assert_eq!(hash(""), 1);
	}

	#[test]
	fn different_contexts_hash_differently() {
		assert_ne!(hash("first"), hash("second"));
		assert_ne!(hash("a"), hash("b"));
		assert_ne!(hash("ab"), hash("ba"));
	}

	// Underscores and dots are legal in a 3.1 context id and must not fold onto letters.
	#[test]
	fn punctuation_is_distinct_from_letters() {
		assert_ne!(hash("a_b"), hash("a.b"));
		assert_ne!(hash("a_b"), hash("ab"));
	}
}
