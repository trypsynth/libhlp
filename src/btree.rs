//! The B+ tree every index in a help file is built from.
//!
//! The internal directory, `|CONTEXT`, `|TTLBTREE` and friends all share one page layout
//! and differ only in what an entry holds, so this module walks the pages and leaves the
//! entry bodies to the caller.

use crate::{
	error::{HlpError, Result},
	format::Reader,
};

/// First two bytes of a `BTREEHEADER`.
const MAGIC: u16 = 0x293B;

/// Bytes in a `BTREEHEADER`, after which the pages start.
const HEADER_LEN: usize = 38;

/// Bytes in the header of a leaf page: unused count, entry count, previous, next.
const LEAF_HEADER_LEN: usize = 8;

/// A parsed B+ tree, positioned over the page array of one internal file.
#[derive(Debug)]
pub struct Btree<'a> {
	/// The internal file's bytes, indexed from the start of the `BTREEHEADER`.
	buf: &'a [u8],
	name: String,
	page_size: usize,
	total_pages: usize,
	root_page: i16,
	levels: u16,
	/// How many entries the header claims, used only to size the result vector.
	total_entries: u32,
}

impl<'a> Btree<'a> {
	/// Parse the `BTREEHEADER` at the start of `buf`. `name` names the internal file in errors.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadBtree`] if the magic, page size, or level count is not usable.
	pub fn parse(buf: &'a [u8], name: &str) -> Result<Self> {
		let bad = || HlpError::BadBtree(name.to_string());
		let mut r = Reader::new(buf);
		if r.u16().map_err(|_| bad())? != MAGIC {
			return Err(bad());
		}
		r.skip(2).map_err(|_| bad())?;
		let page_size = usize::from(r.u16().map_err(|_| bad())?);
		// The Structure string describes the entry layout. Each index this crate reads has
		// a fixed layout already known, so it is skipped along with MustBeZero/PageSplits.
		r.skip(16 + 2 + 2).map_err(|_| bad())?;
		let root_page = r.i16().map_err(|_| bad())?;
		r.skip(2).map_err(|_| bad())?;
		let total_pages = usize::from(r.u16().map_err(|_| bad())?);
		let levels = r.u16().map_err(|_| bad())?;
		let total_entries = r.u32().map_err(|_| bad())?;
		// A page must hold at least a leaf header, and the tree must have a leaf level to
		// reach. Both feed index arithmetic below.
		if page_size < LEAF_HEADER_LEN || levels == 0 {
			return Err(bad());
		}
		Ok(Self { buf, name: name.to_string(), page_size, total_pages, root_page, levels, total_entries })
	}

	/// Read every entry in key order, parsing each with `parse`.
	///
	/// `parse` is called once per entry with a reader positioned at its first byte, and
	/// must consume exactly that entry.
	///
	/// # Errors
	///
	/// Returns [`HlpError::BadBtree`] if a page number is out of range or a page claims
	/// more entries than its bytes hold, or whatever `parse` returns.
	pub fn entries<T, F>(&self, mut parse: F) -> Result<Vec<T>>
	where
		F: FnMut(&mut Reader<'a>) -> Result<T>,
	{
		let bad = || HlpError::BadBtree(self.name.clone());
		let mut out = Vec::with_capacity(self.total_entries.min(1 << 16) as usize);
		let mut page = self.first_leaf()?;
		// Leaf pages are a linked list, and a corrupt NextPage could point back into the
		// list. Visiting at most as many pages as the tree has bounds the walk.
		for _ in 0..=self.total_pages {
			if page < 0 {
				break;
			}
			let bytes = self.page(page)?;
			let mut r = Reader::new(bytes);
			r.skip(2).map_err(|_| bad())?;
			let count = r.i16().map_err(|_| bad())?;
			r.skip(2).map_err(|_| bad())?;
			let next = r.i16().map_err(|_| bad())?;
			for _ in 0..count.max(0) {
				out.push(parse(&mut r)?);
			}
			page = next;
		}
		Ok(out)
	}

	/// Walk down the leftmost path from the root to the first leaf page.
	fn first_leaf(&self) -> Result<i16> {
		let bad = || HlpError::BadBtree(self.name.clone());
		let mut page = self.root_page;
		// The root sits at level `levels`; every level above the leaves is an index page
		// whose PreviousPage leads to the leftmost child.
		for _ in 1..self.levels {
			let bytes = self.page(page)?;
			let mut r = Reader::at(bytes, 4);
			page = r.i16().map_err(|_| bad())?;
		}
		Ok(page)
	}

	/// The bytes of page `n`, which is `page_size` long unless the file is short.
	fn page(&self, n: i16) -> Result<&'a [u8]> {
		let bad = || HlpError::BadBtree(self.name.clone());
		let n = usize::try_from(n).map_err(|_| bad())?;
		if n >= self.total_pages {
			return Err(bad());
		}
		let start = HEADER_LEN.checked_add(n.checked_mul(self.page_size).ok_or(HlpError::Overflow)?).ok_or(
			HlpError::Overflow,
		)?;
		let end = start.checked_add(self.page_size).ok_or(HlpError::Overflow)?;
		// The last page of an internal file can be cut short by UsedSpace, so accept a
		// partial tail rather than rejecting the whole tree.
		self.buf.get(start..end).or_else(|| self.buf.get(start..)).ok_or_else(bad)
	}
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

	use super::*;

	/// Build a tree whose leaf entries are a NUL-terminated name and a little-endian u32.
	fn build(pages: &[Vec<u8>], page_size: usize, root: i16, levels: u16, entries: u32) -> Vec<u8> {
		let mut buf = Vec::new();
		buf.extend_from_slice(&MAGIC.to_le_bytes());
		buf.extend_from_slice(&0x0402u16.to_le_bytes());
		buf.extend_from_slice(&(page_size as u16).to_le_bytes());
		buf.extend_from_slice(&[0u8; 16]);
		buf.extend_from_slice(&0i16.to_le_bytes());
		buf.extend_from_slice(&0i16.to_le_bytes());
		buf.extend_from_slice(&root.to_le_bytes());
		buf.extend_from_slice(&(-1i16).to_le_bytes());
		buf.extend_from_slice(&(pages.len() as u16).to_le_bytes());
		buf.extend_from_slice(&levels.to_le_bytes());
		buf.extend_from_slice(&entries.to_le_bytes());
		for page in pages {
			let mut p = page.clone();
			p.resize(page_size, 0);
			buf.extend_from_slice(&p);
		}
		buf
	}

	fn leaf(names: &[(&str, u32)], prev: i16, next: i16) -> Vec<u8> {
		let mut p = Vec::new();
		p.extend_from_slice(&0u16.to_le_bytes());
		p.extend_from_slice(&(names.len() as i16).to_le_bytes());
		p.extend_from_slice(&prev.to_le_bytes());
		p.extend_from_slice(&next.to_le_bytes());
		for (name, value) in names {
			p.extend_from_slice(name.as_bytes());
			p.push(0);
			p.extend_from_slice(&value.to_le_bytes());
		}
		p
	}

	fn read_entry(r: &mut Reader<'_>) -> Result<(String, u32)> {
		let name = String::from_utf8_lossy(r.cstr()?).into_owned();
		Ok((name, r.u32()?))
	}

	#[test]
	fn a_single_leaf_tree_reads_its_entries() {
		let buf = build(&[leaf(&[("a", 1), ("b", 2)], -1, -1)], 64, 0, 1, 2);
		let tree = Btree::parse(&buf, "|TEST").unwrap();
		assert_eq!(tree.entries(read_entry).unwrap(), vec![("a".into(), 1), ("b".into(), 2)]);
	}

	// Entries live only in leaves, so a multi-level tree has to be descended to the left
	// edge before iterating, not read from the root.
	#[test]
	fn a_two_level_tree_descends_to_the_first_leaf() {
		let mut index = Vec::new();
		index.extend_from_slice(&0u16.to_le_bytes());
		index.extend_from_slice(&1i16.to_le_bytes());
		index.extend_from_slice(&0i16.to_le_bytes());
		index.extend_from_slice(b"c\0");
		index.extend_from_slice(&1i16.to_le_bytes());
		let pages = vec![leaf(&[("a", 1)], -1, 1), leaf(&[("c", 3)], 0, -1), index];
		let buf = build(&pages, 64, 2, 2, 2);
		let tree = Btree::parse(&buf, "|TEST").unwrap();
		assert_eq!(tree.entries(read_entry).unwrap(), vec![("a".into(), 1), ("c".into(), 3)]);
	}

	#[test]
	fn leaf_pages_are_followed_in_order() {
		let pages = vec![leaf(&[("a", 1)], -1, 1), leaf(&[("b", 2)], 0, 2), leaf(&[("c", 3)], 1, -1)];
		let buf = build(&pages, 64, 0, 1, 3);
		let tree = Btree::parse(&buf, "|TEST").unwrap();
		let names: Vec<String> = tree.entries(read_entry).unwrap().into_iter().map(|(n, _)| n).collect();
		assert_eq!(names, ["a", "b", "c"]);
	}

	// A NextPage pointing at an earlier page would otherwise spin forever.
	#[test]
	fn a_cycle_in_the_leaf_list_terminates() {
		let pages = vec![leaf(&[("a", 1)], -1, 1), leaf(&[("b", 2)], 0, 0)];
		let buf = build(&pages, 64, 0, 1, 2);
		let tree = Btree::parse(&buf, "|TEST").unwrap();
		let entries = tree.entries(read_entry).unwrap();
		assert!(entries.len() <= 3);
	}

	#[test]
	fn a_page_number_past_the_end_is_rejected() {
		let buf = build(&[leaf(&[("a", 1)], -1, 5)], 64, 0, 1, 1);
		let tree = Btree::parse(&buf, "|TEST").unwrap();
		assert!(matches!(tree.entries(read_entry), Err(HlpError::BadBtree(_))));
	}

	#[test]
	fn a_bad_header_is_rejected() {
		let mut buf = build(&[leaf(&[("a", 1)], -1, -1)], 64, 0, 1, 1);
		buf[0] = 0;
		assert!(matches!(Btree::parse(&buf, "|TEST"), Err(HlpError::BadBtree(_))));
		let zero_levels = build(&[leaf(&[("a", 1)], -1, -1)], 64, 0, 0, 1);
		assert!(matches!(Btree::parse(&zero_levels, "|TEST"), Err(HlpError::BadBtree(_))));
	}
}
