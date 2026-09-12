//! The LZ77 variant WinHelp uses for topic blocks, phrase tables, and pictures.
//!
//! The stream is a sequence of groups: one flag byte, then up to eight items, one per bit
//! read from the least significant bit up. A clear bit means the next input byte is a
//! literal. A set bit means the next two input bytes are a back-reference whose low 12
//! bits are the distance and whose high 4 bits are the length, less three.

/// Bytes of history a back-reference can name, and the size of the sliding window.
const WINDOW: usize = 0x1000;

/// Shortest run a back-reference can encode, added to the four-bit stored length.
const MIN_MATCH: usize = 3;

/// Decompress `input`, stopping once `max_out` bytes have been produced.
///
/// Truncated or malformed input is not an error: WinHelp pads the tail of a topic block
/// with zeroes and expects the decoder to stop when the real data runs out, so this
/// returns whatever was decoded rather than failing.
pub fn decompress(input: &[u8], max_out: usize) -> Vec<u8> {
	// The window starts zero filled, matching the static buffer the reference decoder
	// uses. A back-reference reaching behind the start of the stream then reads zeroes
	// instead of running off the front, which is what real files that do it expect.
	let mut window = [0u8; WINDOW];
	let mut cursor = 0usize;
	let mut out = Vec::with_capacity(max_out.min(1 << 16));
	let mut pos = 0usize;
	'outer: while pos < input.len() {
		let flags = input[pos];
		pos += 1;
		for bit in 0..8 {
			if pos >= input.len() {
				break 'outer;
			}
			if flags & (1 << bit) == 0 {
				let byte = input[pos];
				pos += 1;
				emit(&mut out, &mut window, &mut cursor, byte, max_out);
				continue;
			}
			let Some(pair) = input.get(pos..pos + 2) else {
				break 'outer;
			};
			pos += 2;
			let pair = u16::from_le_bytes([pair[0], pair[1]]) as usize;
			let len = ((pair >> 12) & 0xf) + MIN_MATCH;
			// Distances are relative to the byte just written, so a stored 0 means
			// "repeat the previous byte".
			let mut back = cursor.wrapping_sub(pair & 0xfff).wrapping_sub(1);
			for _ in 0..len {
				let byte = window[back & (WINDOW - 1)];
				back = back.wrapping_add(1);
				emit(&mut out, &mut window, &mut cursor, byte, max_out);
			}
		}
	}
	out
}

/// Record `byte` in the sliding window, and append it to `out` unless the caller's size
/// limit is already reached.
///
/// The window advances either way. A run that overflows the limit still has to leave the
/// window in the state later back-references expect.
#[inline]
fn emit(out: &mut Vec<u8>, window: &mut [u8; WINDOW], cursor: &mut usize, byte: u8, max_out: usize) {
	window[*cursor & (WINDOW - 1)] = byte;
	*cursor = cursor.wrapping_add(1);
	if out.len() < max_out {
		out.push(byte);
	}
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation)]

	use super::*;

	/// Build a stream from items, packing flag bytes for groups of eight.
	fn encode(items: &[Item]) -> Vec<u8> {
		let mut out = Vec::new();
		for group in items.chunks(8) {
			let mut flags = 0u8;
			for (bit, item) in group.iter().enumerate() {
				if matches!(item, Item::Match { .. }) {
					flags |= 1 << bit;
				}
			}
			out.push(flags);
			for item in group {
				match *item {
					Item::Literal(b) => out.push(b),
					Item::Match { dist, len } => {
						let pair = ((len - MIN_MATCH as u16) << 12) | (dist & 0xfff);
						out.extend_from_slice(&pair.to_le_bytes());
					}
				}
			}
		}
		out
	}

	enum Item {
		Literal(u8),
		Match { dist: u16, len: u16 },
	}

	#[test]
	fn literals_pass_through() {
		let stream = encode(&[Item::Literal(b'a'), Item::Literal(b'b'), Item::Literal(b'c')]);
		assert_eq!(decompress(&stream, 64), b"abc");
	}

	#[test]
	fn a_back_reference_repeats_earlier_output() {
		// "abc" then copy 3 bytes from 3 back.
		let stream =
			encode(&[Item::Literal(b'a'), Item::Literal(b'b'), Item::Literal(b'c'), Item::Match { dist: 2, len: 3 }]);
		assert_eq!(decompress(&stream, 64), b"abcabc");
	}

	// A distance shorter than the run length is how the format encodes a repeated byte, so
	// the copy has to read bytes it is itself writing rather than snapshotting the source.
	#[test]
	fn an_overlapping_back_reference_extends_itself() {
		let stream = encode(&[Item::Literal(b'x'), Item::Match { dist: 0, len: 5 }]);
		assert_eq!(decompress(&stream, 64), b"xxxxxx");
	}

	#[test]
	fn output_stops_at_the_size_limit() {
		let stream = encode(&[Item::Literal(b'x'), Item::Match { dist: 0, len: 15 }]);
		assert_eq!(decompress(&stream, 4), b"xxxx");
	}

	// Nine items means a second flag byte, and getting that boundary wrong shifts every
	// item after the eighth.
	#[test]
	fn a_second_flag_byte_starts_a_new_group() {
		let items: Vec<Item> = (0..9).map(|i| Item::Literal(b'a' + i)).collect();
		assert_eq!(decompress(&encode(&items), 64), b"abcdefghi");
	}

	#[test]
	fn a_truncated_stream_returns_what_it_decoded() {
		let mut stream = encode(&[Item::Literal(b'a'), Item::Literal(b'b'), Item::Match { dist: 1, len: 4 }]);
		stream.pop();
		assert_eq!(decompress(&stream, 64), b"ab");
	}

	#[test]
	fn empty_input_gives_empty_output() {
		assert!(decompress(&[], 64).is_empty());
	}

	// The window is zero filled, so a reference reaching behind the stream reads zeroes
	// instead of panicking on an out-of-range index.
	#[test]
	fn a_reference_before_the_start_reads_zeroes() {
		let stream = encode(&[Item::Match { dist: 100, len: 3 }]);
		assert_eq!(decompress(&stream, 64), [0, 0, 0]);
	}
}
