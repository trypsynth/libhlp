//! A pure-Rust reader for WinHelp (`.hlp`) files.
//!
//! [`HlpFile`] opens a `.hlp` file, reads its internal directory, works out how it was
//! compressed, and hands back its topics as text.
//!
//! ```no_run
//! use libhlp::HlpFile;
//!
//! # fn main() -> libhlp::Result<()> {
//! let hlp = HlpFile::open("manual.hlp")?;
//! println!("{}", hlp.title().unwrap_or("untitled"));
//! for topic in hlp.topics()? {
//!     println!("== {} ==", topic.title);
//!     println!("{}", topic.text());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What a help file is
//!
//! A `.hlp` file is an archive with its own directory, holding internal files with names
//! like `|SYSTEM`, `|TOPIC` and `|Phrases`. Topic text lives in `|TOPIC` as a linked list
//! of records spread across fixed size blocks, LZ77 compressed, and compressed again
//! against a table of common phrases. None of it is RTF on disk, despite WinHelp files
//! having been compiled from RTF: the paragraph structure is a binary command stream that
//! this crate walks to recover text and hotspots.
//!
//! # What a topic holds
//!
//! A [`Topic`] is a sequence of [`Block`]s: paragraphs and tables. A paragraph is a
//! sequence of [`Run`]s, each a stretch of text with its [`Format`] and the hotspot it
//! sits inside, a picture, a tab, or a line break. [`Topic::text`] flattens the lot when
//! the structure is not wanted.
//!
//! Emphasis is not stored with the text. A topic says "switch to font 4", and the `|FONT`
//! table says font 4 is the bold one, so [`Format`] is what that lookup produced. Hotspots
//! are drawn with an underlined font, and that underline is left out of [`Format`], since
//! it is how the link is rendered rather than emphasis the author asked for.
//!
//! [`HlpFile::read_file`] exposes the raw internal files for anything this crate does not
//! interpret, including the bitmaps a [`Run::Image`] names.
//!
//! # Which files are supported
//!
//! WinHelp 3.1 (`HC31`) and Windows 95 (`HCW 4.00`) files, including both phrase
//! compression schemes. WinHelp 3.0 (`HC30`) files report
//! [`HlpError::UnsupportedVersion`]; they use a different topic layout that is not read
//! yet.
//!
//! Files from Borland's help compiler and MS-DOS QuickHelp share the `.hlp` extension but
//! are unrelated formats. They are rejected with [`HlpError::BadMagic`].
//!
//! # Text encoding
//!
//! WinHelp predates Unicode and stores text in a single-byte Windows code page. This crate
//! decodes as Windows-1252, which is right for Western European files.
//! [`HlpFile::charset`] reports what the file claims, so a caller that needs another code
//! page can re-decode the bytes itself.
//!
//! # Robustness
//!
//! Help files are untrusted input. Every offset and length is bounds checked before use,
//! and a damaged topic ends the walk rather than causing a panic, so the topics already
//! read are still returned.

#![warn(missing_docs)]

mod btree;
mod error;
mod font;
mod format;
mod hlp;
mod link;
mod lz77;
mod paragraph;
mod phrase;
mod system;
mod table;
mod text;
mod topic;

pub use error::{HlpError, Result};
pub use font::{Font, Fonts, Format};
pub use hlp::HlpFile;
pub use link::{Link, Target, hash};
pub use paragraph::{Paragraph, Run};
pub use table::{Block, Cell, Row, Table};
pub use topic::Topic;
