//! Decoding one displayable topic record into paragraphs.
//!
//! A record carries two streams. `LinkData1` holds paragraph settings followed by a list
//! of formatting commands; `LinkData2` holds the text, as one NUL-terminated string per
//! command. Reading them means alternating: emit the next string, act on the next command,
//! repeat until the command stream says it is done.

use crate::{
	font::{Fonts, Format},
	format::Reader,
	link::{Link, Target},
	table::{Block, TableBuilder},
	text,
};

/// A WinHelp 3.0 displayable record.
pub const TL_DISPLAY30: u8 = 0x01;

/// A topic header record, which starts a new topic rather than carrying text.
pub const TL_TOPICHDR: u8 = 0x02;

/// A WinHelp 3.1 displayable record.
pub const TL_DISPLAY: u8 = 0x20;

/// A WinHelp 3.1 table record, whose cells each carry their own paragraph settings.
pub const TL_TABLE: u8 = 0x23;

/// One piece of a paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
	/// A stretch of text, carrying its emphasis and the hotspot it sits inside if it is
	/// in one.
	Text {
		/// The text itself, already decoded.
		text: String,
		/// The emphasis the text is set in.
		format: Format,
		/// Where clicking this text would go, if it is a hotspot.
		link: Option<Link>,
	},
	/// A picture.
	///
	/// `Some(n)` when the bitmap is stored as the internal file `|bmN`, which
	/// [`HlpFile::read_file`] will hand back. `None` when it is embedded in the topic
	/// itself, which this crate does not decode.
	///
	/// [`HlpFile::read_file`]: crate::HlpFile::read_file
	Image(Option<u16>),
	/// A jump to the next tab stop.
	Tab,
	/// A forced break that does not start a new paragraph.
	LineBreak,
}

/// One paragraph of a topic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paragraph {
	/// The pieces the paragraph is built from, in reading order.
	pub runs: Vec<Run>,
}

impl Paragraph {
	/// The paragraph's text, with tabs and line breaks as their own characters and
	/// pictures left out.
	#[must_use]
	pub fn text(&self) -> String {
		let mut out = String::new();
		for run in &self.runs {
			match run {
				Run::Text { text, .. } => out.push_str(text),
				Run::Tab => out.push('\t'),
				Run::LineBreak => out.push('\n'),
				Run::Image(_) => {}
			}
		}
		out
	}

	/// Whether the paragraph would render as nothing at all.
	#[must_use]
	pub const fn is_empty(&self) -> bool {
		self.runs.is_empty()
	}

	/// Every hotspot in the paragraph, in reading order.
	pub fn links(&self) -> impl Iterator<Item = &Link> {
		self.runs.iter().filter_map(|run| match run {
			Run::Text { link, .. } => link.as_ref(),
			_ => None,
		})
	}
}

/// What one displayable record contributed.
pub struct Record {
	/// The blocks it held: loose paragraphs, or one table.
	pub blocks: Vec<Block>,
	/// The record's `TopicLength`, which advances the file's topic offset counter.
	pub topic_length: u32,
}

/// Bits in a paragraph's settings word saying which optional fields follow it.
mod settings {
	pub const UNKNOWN: u16 = 0x0001;
	pub const SPACING_ABOVE: u16 = 0x0002;
	pub const SPACING_BELOW: u16 = 0x0004;
	pub const SPACING_LINES: u16 = 0x0008;
	pub const LEFT_INDENT: u16 = 0x0010;
	pub const RIGHT_INDENT: u16 = 0x0020;
	pub const FIRSTLINE_INDENT: u16 = 0x0040;
	pub const BORDER: u16 = 0x0100;
	pub const TABS: u16 = 0x0200;
	/// A tab stop with this bit set is followed by its alignment.
	pub const TAB_HAS_TYPE: u16 = 0x4000;
}

/// Decode one displayable record.
///
/// Malformed command or text streams end the record rather than failing the file: a help
/// file with one bad topic should still give up the rest of its topics.
pub fn parse(record_type: u8, data1: &[u8], data2: &[u8], fonts: &Fonts) -> Record {
	let mut cmds = Reader::new(data1);
	let mut strings = Reader::new(data2);
	let mut out = Paragraphs::new(fonts);
	// TopicSize, which counts bytes and is of no use once the record is already in hand.
	let _ = cmds.clong();
	let topic_length =
		if matches!(record_type, TL_DISPLAY | TL_TABLE) { u32::from(cmds.cushort().unwrap_or(0)) } else { 0 };
	if record_type != TL_TABLE {
		read_block(&mut cmds, &mut strings, &mut out, data1.len());
		return Record { blocks: out.finish().into_iter().map(Block::Paragraph).collect(), topic_length };
	}
	let mut table = TableBuilder::default();
	if skip_table_header(&mut cmds).is_some() {
		// Cells run one after another, each opening with the column it sits in. Reading one
		// leaves the cursor on the next cell's column, or on the -1 that ends the table.
		while let Ok(column) = Reader::at(data1, cmds.pos()).i16() {
			// Column, an unknown short, and a byte that is always zero.
			if column == -1 || cmds.skip(5).is_err() {
				break;
			}
			let read = read_block(&mut cmds, &mut strings, &mut out, data1.len());
			table.push(column, out.take());
			if !read {
				break;
			}
		}
	}
	let table = table.finish();
	let blocks = if table.rows.is_empty() { Vec::new() } else { vec![Block::Table(table)] };
	Record { blocks, topic_length }
}

/// Read one block of paragraph settings and the commands that follow it, up to the
/// end-of-formatting command. False when either stream ran out first.
fn read_block(cmds: &mut Reader<'_>, strings: &mut Reader<'_>, out: &mut Paragraphs<'_>, len: usize) -> bool {
	if cmds.pos() >= len || skip_settings(cmds).is_none() {
		return false;
	}
	read_commands(cmds, strings, out).is_some()
}

/// Step over a table record's column geometry, which precedes its first cell.
fn skip_table_header(cmds: &mut Reader<'_>) -> Option<()> {
	let columns = cmds.u8().ok()?;
	let table_type = cmds.u8().ok()?;
	// Variable width tables state a minimum; fixed width ones do not.
	if table_type == 0 || table_type == 2 {
		cmds.i16().ok()?;
	}
	// A gap width and a column width per column.
	cmds.skip(4 * usize::from(columns)).ok()
}

/// Step over one paragraph's settings, whose size depends on the bits in its flags word.
fn skip_settings(cmds: &mut Reader<'_>) -> Option<()> {
	// Two unknown bytes and the paragraph id.
	cmds.skip(4).ok()?;
	let bits = cmds.u16().ok()?;
	if bits & settings::UNKNOWN != 0 {
		cmds.clong().ok()?;
	}
	for bit in [
		settings::SPACING_ABOVE,
		settings::SPACING_BELOW,
		settings::SPACING_LINES,
		settings::LEFT_INDENT,
		settings::RIGHT_INDENT,
		settings::FIRSTLINE_INDENT,
	] {
		if bits & bit != 0 {
			cmds.cshort().ok()?;
		}
	}
	if bits & settings::BORDER != 0 {
		// Border flags, then the border width.
		cmds.skip(3).ok()?;
	}
	if bits & settings::TABS != 0 {
		let count = cmds.cshort().ok()?;
		for _ in 0..count.max(0) {
			let stop = cmds.cushort().ok()?;
			if stop & settings::TAB_HAS_TYPE != 0 {
				cmds.cushort().ok()?;
			}
		}
	}
	Some(())
}

/// Read text and commands in lockstep until the end-of-formatting command.
///
/// Returns `None` when either stream runs out, which ends the record.
fn read_commands(cmds: &mut Reader<'_>, strings: &mut Reader<'_>, out: &mut Paragraphs<'_>) -> Option<()> {
	loop {
		out.push_text(strings.cstr().ok()?);
		let cmd = cmds.u8().ok()?;
		match cmd {
			0xFF => return Some(()),
			// vfld and dtype, both media view field references with no text of their own.
			0x20 => cmds.skip(4).ok()?,
			// A dtype field reference, which carries one short and no text.
			0x21 => cmds.skip(2).ok()?,
			0x80 => {
				let font = cmds.u16().ok()?;
				out.set_font(font);
			}
			0x81 => out.push(Run::LineBreak),
			// Ends a paragraph, not a table cell. A cell ends with the end-of-formatting
			// command like any other block, and the outer loop picks up the next one.
			0x82 => out.end_paragraph(),
			0x83 => out.push(Run::Tab),
			0x86..=0x88 => skip_picture(cmds, out)?,
			0x89 => out.end_link(),
			// A macro hotspot, whose text is the macro to run.
			0xC8 | 0xCC => {
				let body = counted_body(cmds)?;
				out.start_link(Link::jump(Target::Macro(text::decode_cstr(body))));
			}
			// A WinHelp 3.0 jump, which names its target by topic number.
			0xE0 | 0xE1 => {
				let number = cmds.u32().ok()?;
				out.start_link(Link { target: Target::Number(number), popup: cmd == 0xE0 });
			}
			// A WinHelp 3.1 jump. The four bytes are not a topic offset but the hash of the
			// target's context string, which `|CONTEXT` maps to one.
			0xE2 | 0xE3 | 0xE6 | 0xE7 => {
				let context = cmds.u32().ok()?;
				let popup = cmd == 0xE2 || cmd == 0xE6;
				out.start_link(Link { target: Target::Context(context), popup });
			}
			// A jump that may leave this file or open a secondary window.
			0xEA | 0xEB | 0xEE | 0xEF => {
				let body = counted_body(cmds)?;
				let popup = cmd == 0xEA || cmd == 0xEB;
				out.start_link(parse_external(body, popup));
			}
			// A non-breaking space or hyphen, whose character is already in the text
			// stream, and anything unrecognised, which WinHelp also treats as one byte.
			_ => {}
		}
	}
}

/// A picture command whose payload is a bitmap, rather than an embedded window.
const PICTURE_HC30: u8 = 0x03;

/// The WinHelp 3.1 form, which counts its hotspots before the bitmap itself.
const PICTURE_HC31: u8 = 0x22;

/// Step over an inline picture, recording where its bitmap lives.
///
/// The payload may be an embedded window, a button, or a media command rather than a
/// picture. Those are stepped over without a run, since none of them is an image.
fn skip_picture(cmds: &mut Reader<'_>, out: &mut Paragraphs<'_>) -> Option<()> {
	let kind = cmds.u8().ok()?;
	let size = usize::try_from(cmds.clong().ok()?).unwrap_or(0);
	if kind == PICTURE_HC31 {
		// How many hotspots the picture holds. It counts towards topic offsets, which
		// this crate tracks per record rather than per character.
		cmds.cushort().ok()?;
	}
	let payload = cmds.take(size).ok()?;
	if kind != PICTURE_HC30 && kind != PICTURE_HC31 {
		return Some(());
	}
	// The payload opens with a flag saying whether the bitmap follows inline; when it does
	// not, the short after it is the number of the `|bmN` file holding it.
	let embedded = payload.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
	let number = match embedded {
		Some(0) => payload.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]])),
		_ => None,
	};
	out.push(Run::Image(number));
	Some(())
}

/// Read a command body introduced by its own length, and step the reader past it.
///
/// The length sits in the two bytes after the command and counts from the command byte,
/// so the body is three bytes in and the whole command is the length plus three.
fn counted_body<'a>(cmds: &mut Reader<'a>) -> Option<&'a [u8]> {
	let start = cmds.pos();
	let len = cmds.i16().ok()?;
	let total = usize::try_from(len).ok()?.checked_add(3)?;
	// Both fields already read are part of that total, so only the remainder is the body.
	let body_len = total.checked_sub(3)?;
	let body = cmds.take(body_len).ok()?;
	cmds.seek(start.checked_sub(1)?.checked_add(total)?);
	Some(body)
}

/// Interpret the body of an external or secondary-window jump.
fn parse_external(body: &[u8], popup: bool) -> Link {
	let mut r = Reader::new(body);
	let Ok(kind) = r.u8() else { return Link::jump(Target::Context(0)) };
	let context = r.u32().unwrap_or(0);
	// Type 4 and 6 name another help file; 0 and 1 open a window on this one.
	let file = match kind {
		4 | 6 => text::decode_cstr(r.cstr().unwrap_or_default()),
		_ => String::new(),
	};
	let target = if file.is_empty() { Target::Context(context) } else { Target::External { file, context } };
	Link { target, popup }
}

/// Collects runs into paragraphs while a record is read.
struct Paragraphs<'a> {
	fonts: &'a Fonts,
	done: Vec<Paragraph>,
	current: Paragraph,
	link: Option<Link>,
	format: Format,
}

impl<'a> Paragraphs<'a> {
	fn new(fonts: &'a Fonts) -> Self {
		Self { fonts, done: Vec::new(), current: Paragraph::default(), link: None, format: Format::default() }
	}

	fn push(&mut self, run: Run) {
		self.current.runs.push(run);
	}

	/// Add a text run, dropping the empty strings that stand in for a bare command.
	fn push_text(&mut self, bytes: &[u8]) {
		if bytes.is_empty() {
			return;
		}
		let mut format = self.format;
		// WinHelp draws a hotspot by switching to an underlined font, so the underline is
		// how the link is rendered rather than emphasis the author asked for. Reporting
		// both would have a reader announce every link twice over.
		if self.link.is_some() {
			format.underline = false;
		}
		self.push(Run::Text { text: text::decode(bytes), format, link: self.link.clone() });
	}

	fn set_font(&mut self, font: u16) {
		self.format = self.fonts.format(font);
	}

	fn start_link(&mut self, link: Link) {
		self.link = Some(link);
	}

	fn end_link(&mut self) {
		self.link = None;
	}

	fn end_paragraph(&mut self) {
		// A hotspot never survives its paragraph, and leaving one open would attach it to
		// unrelated text further down the topic.
		self.link = None;
		self.done.push(std::mem::take(&mut self.current));
	}

	/// Take the paragraphs read so far, leaving the collector ready for the next cell.
	///
	/// A cell's last paragraph is usually still open, since the end-of-formatting command
	/// closes the cell rather than the paragraph.
	fn take(&mut self) -> Vec<Paragraph> {
		if !self.current.is_empty() {
			self.done.push(std::mem::take(&mut self.current));
		}
		// Nothing carries from one cell into the next.
		self.link = None;
		self.format = Format::default();
		std::mem::take(&mut self.done)
	}

	fn finish(mut self) -> Vec<Paragraph> {
		if !self.current.is_empty() {
			self.done.push(self.current);
		}
		self.done
	}
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

	use super::*;

	/// Build the `LinkData1` prologue shared by every 3.1 display record: a topic size, a
	/// topic length, and one block of paragraph settings with no optional fields.
	fn display_prologue(topic_length: u16) -> Vec<u8> {
		let mut buf = Vec::new();
		// A compressed long of 0x4000 encodes as the two byte form holding zero.
		buf.extend_from_slice(&0x8000u16.to_le_bytes());
		buf.push((topic_length * 2) as u8);
		buf.extend_from_slice(&[0, 0, 0, 0]);
		buf.extend_from_slice(&0u16.to_le_bytes());
		buf
	}

	/// Parse a record against an empty font table, for the cases that ignore emphasis.
	fn plain(record_type: u8, data1: &[u8], data2: &[u8]) -> Record {
		parse(record_type, data1, data2, &Fonts::default())
	}

	impl Record {
		/// Every paragraph the record produced, table cells included.
		fn paragraphs(&self) -> Vec<&Paragraph> {
			self.blocks.iter().flat_map(Block::paragraphs).collect()
		}
	}

	fn strings(parts: &[&str]) -> Vec<u8> {
		let mut buf = Vec::new();
		for part in parts {
			buf.extend_from_slice(part.as_bytes());
			buf.push(0);
		}
		buf
	}

	#[test]
	fn a_single_paragraph_reads_its_text() {
		let mut cmds = display_prologue(6);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["Hello."]));
		assert_eq!(record.topic_length, 6);
		assert_eq!(record.paragraphs().len(), 1);
		assert_eq!(record.paragraphs()[0].text(), "Hello.");
	}

	// One record usually holds several paragraphs, split by the end-of-paragraph command
	// rather than by another settings block.
	#[test]
	fn end_of_paragraph_splits_the_record() {
		let mut cmds = display_prologue(0);
		cmds.push(0x82);
		cmds.push(0x82);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["One", "Two", "Three"]));
		let texts: Vec<String> = record.paragraphs().into_iter().map(Paragraph::text).collect();
		assert_eq!(texts, ["One", "Two", "Three"]);
	}

	#[test]
	fn line_breaks_and_tabs_become_runs() {
		let mut cmds = display_prologue(0);
		cmds.push(0x83);
		cmds.push(0x81);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["a", "b", "c"]));
		assert_eq!(record.paragraphs()[0].text(), "a\tb\nc");
	}

	/// A font table whose font 1 is bold and font 2 is underlined.
	fn test_fonts() -> Fonts {
		let mut buf = Vec::new();
		buf.extend_from_slice(&0u16.to_le_bytes());
		buf.extend_from_slice(&3u16.to_le_bytes());
		buf.extend_from_slice(&8u16.to_le_bytes());
		buf.extend_from_slice(&8u16.to_le_bytes());
		for attrs in [0u8, 0x01, 0x04] {
			buf.push(attrs);
			buf.extend_from_slice(&[20, 3, 0, 0, 0, 0, 0, 0, 0, 0]);
		}
		Fonts::parse(&buf).unwrap()
	}

	/// The bytes of a font-change command selecting `font`.
	fn font_change(font: u16) -> [u8; 3] {
		let b = font.to_le_bytes();
		[0x80, b[0], b[1]]
	}

	// Emphasis is not in the text stream at all: it comes from looking the font number up
	// in |FONT. This is the whole point of reading that file.
	#[test]
	fn a_font_change_applies_its_emphasis_to_later_text() {
		let mut cmds = display_prologue(0);
		cmds.extend_from_slice(&font_change(1));
		cmds.extend_from_slice(&font_change(0));
		cmds.push(0xFF);
		let record = parse(TL_DISPLAY, &cmds, &strings(&["plain ", "bold ", "plain again"]), &test_fonts());
		let runs = &record.paragraphs()[0].runs;
		assert_eq!(runs[0], Run::Text { text: "plain ".into(), format: Format::default(), link: None });
		let bold = Format { bold: true, ..Format::default() };
		assert_eq!(runs[1], Run::Text { text: "bold ".into(), format: bold, link: None });
		assert_eq!(runs[2], Run::Text { text: "plain again".into(), format: Format::default(), link: None });
	}

	// WinHelp draws hotspots by switching to an underlined font. That underline is the
	// link's rendering, not emphasis, so reporting it would make a reader announce every
	// link as underlined on top of announcing it as a link.
	#[test]
	fn a_hotspots_underline_is_not_reported_as_emphasis() {
		let mut cmds = display_prologue(0);
		cmds.extend_from_slice(&font_change(2));
		cmds.push(0xE3);
		cmds.extend_from_slice(&7u32.to_le_bytes());
		cmds.push(0x89);
		cmds.push(0xFF);
		// One string is emitted before each command, so the two bare commands ahead of the
		// hotspot text each consume an empty one.
		let record = parse(TL_DISPLAY, &cmds, &strings(&["", "", "click", " after"]), &test_fonts());
		let runs = &record.paragraphs()[0].runs;
		let Run::Text { format, link: Some(_), .. } = &runs[0] else { panic!("expected a hotspot") };
		assert!(!format.underline);
		// The font is still underlined once the hotspot ends, so the suppression is scoped
		// to the link rather than clearing the font.
		let Run::Text { format, link: None, .. } = &runs[1] else { panic!("expected plain text") };
		assert!(format.underline);
	}

	// A font change carries a two byte argument. Miscounting it would make every command
	// after the first font change read from the wrong byte.
	#[test]
	fn a_font_change_is_stepped_over() {
		let mut cmds = display_prologue(0);
		cmds.extend_from_slice(&[0x80, 0x03, 0x00]);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["before", "after"]));
		assert_eq!(record.paragraphs()[0].text(), "beforeafter");
	}

	#[test]
	fn a_topic_jump_marks_its_text_as_a_hotspot() {
		let mut cmds = display_prologue(0);
		cmds.push(0xE3);
		cmds.extend_from_slice(&0x1234u32.to_le_bytes());
		cmds.push(0x89);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["see ", "here", " now"]));
		let runs = &record.paragraphs()[0].runs;
		assert_eq!(runs[0], Run::Text { text: "see ".into(), format: Format::default(), link: None });
		assert_eq!(
			runs[1],
			Run::Text {
				text: "here".into(),
				format: Format::default(),
				link: Some(Link::jump(Target::Context(0x1234)))
			}
		);
		assert_eq!(runs[2], Run::Text { text: " now".into(), format: Format::default(), link: None });
	}

	#[test]
	fn a_popup_jump_is_told_apart_from_a_topic_jump() {
		let mut cmds = display_prologue(0);
		cmds.push(0xE2);
		cmds.extend_from_slice(&7u32.to_le_bytes());
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["", "pop"]));
		let expected = Link::popup(Target::Context(7));
		assert_eq!(
			record.paragraphs()[0].runs[0],
			Run::Text { text: "pop".into(), format: Format::default(), link: Some(expected) }
		);
	}

	#[test]
	fn a_macro_hotspot_keeps_its_macro_text() {
		let mut cmds = display_prologue(0);
		cmds.push(0xC8);
		let macro_text = b"JI(`x.hlp',`topic')\0";
		cmds.extend_from_slice(&(macro_text.len() as i16).to_le_bytes());
		cmds.extend_from_slice(macro_text);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["", "click"]));
		let Run::Text { link: Some(Link { target: Target::Macro(m), .. }), .. } = &record.paragraphs()[0].runs[0]
		else {
			panic!("expected a macro hotspot");
		};
		assert_eq!(m, "JI(`x.hlp',`topic')");
	}

	#[test]
	fn a_jump_into_another_file_keeps_the_file_name() {
		let mut cmds = display_prologue(0);
		cmds.push(0xEE);
		let mut body = vec![4u8];
		body.extend_from_slice(&99u32.to_le_bytes());
		body.extend_from_slice(b"other.hlp\0");
		cmds.extend_from_slice(&(body.len() as i16).to_le_bytes());
		cmds.extend_from_slice(&body);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["", "go"]));
		let Run::Text { link: Some(Link { target: Target::External { file, context }, popup }), .. } =
			&record.paragraphs()[0].runs[0]
		else {
			panic!("expected an external jump");
		};
		assert_eq!(file, "other.hlp");
		assert_eq!(*context, 99);
		assert!(!popup);
	}

	/// A picture command carrying `payload`, whose size is a compressed long.
	fn picture(kind: u8, payload: &[u8]) -> Vec<u8> {
		let mut out = vec![0x86, kind];
		let size = ((payload.len() as u16) + 0x4000) * 2;
		out.extend_from_slice(&size.to_le_bytes());
		out.extend_from_slice(payload);
		out
	}

	// A picture states its own size, and the reader has to jump the whole thing or every
	// later command reads picture bytes as commands.
	#[test]
	fn a_picture_is_stepped_over_and_recorded() {
		let mut cmds = display_prologue(0);
		// Not embedded, so the short after the flag is the |bmN file number.
		cmds.extend_from_slice(&picture(0x03, &[0, 0, 3, 0]));
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["before", "after"]));
		assert_eq!(record.paragraphs()[0].runs[1], Run::Image(Some(3)));
		assert_eq!(record.paragraphs()[0].text(), "beforeafter");
	}

	// A bitmap stored inline has no |bmN file to point at, and this crate does not decode
	// the bytes, so the run says a picture was there and nothing more.
	#[test]
	fn an_embedded_picture_has_no_file_number() {
		let mut cmds = display_prologue(0);
		cmds.extend_from_slice(&picture(0x03, &[1, 0, 0xde, 0xad]));
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["", ""]));
		assert_eq!(record.paragraphs()[0].runs[0], Run::Image(None));
	}

	// The same commands also carry embedded windows, buttons and media clips. None of them
	// is a picture, so none should turn up as one.
	#[test]
	fn an_embedded_window_is_not_reported_as_a_picture() {
		let mut cmds = display_prologue(0);
		cmds.extend_from_slice(&picture(0x05, &[0, 0, 0, 0, 0, 0, b'x', 0]));
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["text", ""]));
		let expected = Run::Text { text: "text".into(), format: Format::default(), link: None };
		assert_eq!(record.paragraphs()[0].runs, vec![expected]);
	}

	// The 3.1 form counts its hotspots between the size and the payload. Missing that field
	// shifts the payload and loses the bitmap number.
	#[test]
	fn the_31_picture_form_skips_its_hotspot_count() {
		let mut cmds = display_prologue(0);
		let mut pic = vec![0x86, 0x22];
		pic.extend_from_slice(&((4u16 + 0x4000) * 2).to_le_bytes());
		// A compressed unsigned short of 2, then the payload.
		pic.push(4);
		pic.extend_from_slice(&[0, 0, 7, 0]);
		cmds.extend_from_slice(&pic);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["", ""]));
		assert_eq!(record.paragraphs()[0].runs[0], Run::Image(Some(7)));
	}

	// A WinHelp 3.0 record has no topic length field, so reading one would eat a byte of
	// the settings block.
	#[test]
	fn a_30_record_has_no_topic_length() {
		let mut cmds = Vec::new();
		cmds.extend_from_slice(&0x8000u16.to_le_bytes());
		cmds.extend_from_slice(&[0, 0, 0, 0]);
		cmds.extend_from_slice(&0u16.to_le_bytes());
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY30, &cmds, &strings(&["Old."]));
		assert_eq!(record.topic_length, 0);
		assert_eq!(record.paragraphs()[0].text(), "Old.");
	}

	// A record whose commands run out mid-stream still yields the text already decoded,
	// including the paragraph that was open when the commands ended.
	#[test]
	fn a_truncated_command_stream_keeps_what_it_read() {
		let mut cmds = display_prologue(0);
		cmds.push(0x82);
		let record = plain(TL_DISPLAY, &cmds, &strings(&["Kept", "Also kept"]));
		let texts: Vec<String> = record.paragraphs().into_iter().map(Paragraph::text).collect();
		assert_eq!(texts, ["Kept", "Also kept"]);
	}

	#[test]
	fn a_record_with_no_text_yields_no_paragraphs() {
		let mut cmds = display_prologue(0);
		cmds.push(0xFF);
		let record = plain(TL_DISPLAY, &cmds, &strings(&[""]));
		assert!(record.paragraphs().is_empty());
	}
}
