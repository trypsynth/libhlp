//! Tables, and the block sequence a topic is built from.
//!
//! A table record stores its cells one after another, each with its own paragraph settings
//! and its own column number. Rows are not marked: a cell whose column does not advance
//! has started a new one. [`TableBuilder`] turns that flat run of cells back into rows.

use crate::{link::Link, paragraph::Paragraph};

/// One cell of a table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cell {
	/// The cell's contents. A cell may hold several paragraphs, or none at all.
	pub paragraphs: Vec<Paragraph>,
}

impl Cell {
	/// The cell's text, one paragraph per line.
	#[must_use]
	pub fn text(&self) -> String {
		join_paragraphs(&self.paragraphs)
	}
}

/// One row of a table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
	/// The row's cells, in column order.
	pub cells: Vec<Cell>,
}

impl Row {
	/// The row's text, with a tab between cells.
	#[must_use]
	pub fn text(&self) -> String {
		self.cells.iter().map(Cell::text).collect::<Vec<_>>().join("\t")
	}
}

/// A table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
	/// The table's rows, in reading order.
	pub rows: Vec<Row>,
}

impl Table {
	/// The table's text, a tab between cells and a newline between rows.
	#[must_use]
	pub fn text(&self) -> String {
		self.rows.iter().map(Row::text).collect::<Vec<_>>().join("\n")
	}

	/// Every paragraph in the table, in reading order.
	pub fn paragraphs(&self) -> impl Iterator<Item = &Paragraph> {
		self.rows.iter().flat_map(|r| &r.cells).flat_map(|c| &c.paragraphs)
	}
}

/// One piece of a topic. A topic is a sequence of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
	/// An ordinary paragraph.
	Paragraph(Paragraph),
	/// A table.
	Table(Table),
}

impl Block {
	/// The block's text.
	#[must_use]
	pub fn text(&self) -> String {
		match self {
			Self::Paragraph(p) => p.text(),
			Self::Table(t) => t.text(),
		}
	}

	/// Every paragraph the block holds, in reading order.
	#[must_use]
	pub fn paragraphs(&self) -> Box<dyn Iterator<Item = &Paragraph> + '_> {
		match self {
			Self::Paragraph(p) => Box::new(std::iter::once(p)),
			Self::Table(t) => Box::new(t.paragraphs()),
		}
	}

	/// Every hotspot the block holds, in reading order.
	pub fn links(&self) -> impl Iterator<Item = &Link> {
		self.paragraphs().flat_map(Paragraph::links)
	}
}

/// Assembles a table's cells into rows as they are read.
#[derive(Debug, Default)]
pub struct TableBuilder {
	rows: Vec<Row>,
	row: Row,
	/// Column number of the cell just added, so the next one can be compared against it.
	last_column: Option<i16>,
}

impl TableBuilder {
	/// Add the cell at `column`, starting a new row if the column did not advance.
	pub fn push(&mut self, column: i16, paragraphs: Vec<Paragraph>) {
		if self.last_column.is_some_and(|last| column <= last) {
			self.end_row();
		}
		self.last_column = Some(column);
		self.row.cells.push(Cell { paragraphs });
	}

	/// Close the open row, dropping it if nothing in it would render.
	///
	/// Help authors use one-cell tables to indent a block of text, and those land next to
	/// a real table as rows with nothing in them. A row where some cells are empty and
	/// others are not is a real row and is kept.
	fn end_row(&mut self) {
		let row = std::mem::take(&mut self.row);
		if row.cells.iter().any(|c| !c.text().trim().is_empty()) {
			self.rows.push(row);
		}
	}

	/// The finished table.
	#[must_use]
	pub fn finish(mut self) -> Table {
		self.end_row();
		Table { rows: self.rows }
	}
}

/// Join paragraphs with a newline apiece, which is how every text view here reads.
fn join_paragraphs(paragraphs: &[Paragraph]) -> String {
	paragraphs.iter().map(Paragraph::text).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{font::Format, paragraph::Run};

	fn cell(text: &str) -> Vec<Paragraph> {
		vec![Paragraph { runs: vec![Run::Text { text: text.into(), format: Format::default(), link: None }] }]
	}

	// Rows are not marked in the file. The only signal is the column number going back
	// down, so a two by two table arrives as four cells numbered 0, 1, 0, 1.
	#[test]
	fn a_column_that_does_not_advance_starts_a_row() {
		let mut b = TableBuilder::default();
		b.push(0, cell("a"));
		b.push(1, cell("b"));
		b.push(0, cell("c"));
		b.push(1, cell("d"));
		let table = b.finish();
		assert_eq!(table.rows.len(), 2);
		assert_eq!(table.rows[0].cells.len(), 2);
		assert_eq!(table.text(), "a\tb\nc\td");
	}

	#[test]
	fn one_row_stays_one_row() {
		let mut b = TableBuilder::default();
		b.push(0, cell("only"));
		b.push(1, cell("row"));
		let table = b.finish();
		assert_eq!(table.rows.len(), 1);
		assert_eq!(table.text(), "only\trow");
	}

	// A single column table repeats column 0, so every cell has to open its own row.
	#[test]
	fn a_single_column_table_is_all_rows() {
		let mut b = TableBuilder::default();
		b.push(0, cell("one"));
		b.push(0, cell("two"));
		b.push(0, cell("three"));
		assert_eq!(b.finish().text(), "one\ntwo\nthree");
	}

	// A table can skip a column when a cell is merged or empty, and that is still the same
	// row as long as the number keeps climbing.
	#[test]
	fn a_skipped_column_stays_in_the_same_row() {
		let mut b = TableBuilder::default();
		b.push(0, cell("a"));
		b.push(2, cell("c"));
		assert_eq!(b.finish().rows.len(), 1);
	}

	// A one-cell table used to indent text lands next to a real table, and a row with
	// nothing in it would read as a blank line in the middle of the data.
	#[test]
	fn a_row_with_nothing_in_it_is_dropped() {
		let mut b = TableBuilder::default();
		b.push(0, cell(""));
		b.push(0, cell("real"));
		let table = b.finish();
		assert_eq!(table.rows.len(), 1);
		assert_eq!(table.text(), "real");
	}

	// An empty cell beside a full one is a real cell of a real row, not padding.
	#[test]
	fn a_row_with_one_empty_cell_is_kept() {
		let mut b = TableBuilder::default();
		b.push(0, cell(""));
		b.push(1, cell("value"));
		assert_eq!(b.finish().text(), "	value");
	}

	#[test]
	fn an_empty_table_has_no_rows() {
		assert_eq!(TableBuilder::default().finish(), Table::default());
		assert_eq!(Table::default().text(), "");
	}

	#[test]
	fn a_block_reports_the_text_of_whichever_kind_it_is() {
		let paragraph = Block::Paragraph(Paragraph { runs: cell("plain")[0].runs.clone() });
		assert_eq!(paragraph.text(), "plain");
		assert_eq!(paragraph.paragraphs().count(), 1);
		let mut b = TableBuilder::default();
		b.push(0, cell("x"));
		b.push(1, cell("y"));
		let table = Block::Table(b.finish());
		assert_eq!(table.text(), "x\ty");
		assert_eq!(table.paragraphs().count(), 2);
	}
}
