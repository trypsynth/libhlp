//! Error types returned by this crate.

use std::{io, result};

use thiserror::Error;

/// The error type for all fallible operations in this crate.
#[derive(Debug, Error)]
pub enum HlpError {
	/// An underlying I/O operation failed.
	#[error("I/O error: {0}")]
	Io(#[from] io::Error),
	/// The file does not start with the WinHelp magic number.
	#[error("not a WinHelp file: bad magic number")]
	BadMagic,
	/// The file header is present but describes a file shorter than the bytes actually read.
	#[error("truncated help file")]
	Truncated,
	/// An internal file header is malformed.
	#[error("bad internal file header at offset {0}")]
	BadFileHeader(u64),
	/// A B+ tree header or page is malformed.
	#[error("malformed B+ tree in internal file {0}")]
	BadBtree(String),
	/// No internal file exists under the requested name.
	#[error("internal file not found: {0}")]
	NotFound(String),
	/// The `|SYSTEM` internal file is missing or malformed.
	#[error("bad |SYSTEM record: {0}")]
	BadSystem(&'static str),
	/// The `|FONT` internal file is malformed.
	#[error("bad |FONT file: {0}")]
	BadFont(&'static str),
	/// The `|TOPIC` internal file is malformed.
	#[error("bad |TOPIC data: {0}")]
	BadTopic(&'static str),
	/// The phrase table used for phrase or Hall decompression is malformed.
	#[error("bad phrase table: {0}")]
	BadPhrases(&'static str),
	/// A length or offset calculation would overflow the target integer type.
	#[error("integer overflow in offset calculation")]
	Overflow,
}

/// A specialized [`std::result::Result`] type using [`HlpError`].
pub type Result<T> = result::Result<T, HlpError>;
