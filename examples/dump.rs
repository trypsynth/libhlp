//! Print what libhlp reads out of a help file.
//!
//! ```sh
//! cargo run --example dump -- manual.hlp
//! ```

use std::{env, process};

use libhlp::{Block, HlpFile, Run};

fn main() {
	let Some(path) = env::args().nth(1) else {
		eprintln!("usage: dump <file.hlp>");
		process::exit(2);
	};
	let hlp = match HlpFile::open(&path) {
		Ok(hlp) => hlp,
		Err(e) => {
			eprintln!("{path}: {e}");
			process::exit(1);
		}
	};
	println!("title:    {}", hlp.title().unwrap_or("(none)"));
	println!("version:  {}", hlp.version());
	println!("charset:  {:?}", hlp.charset());
	let mut names: Vec<&str> = hlp.file_names().collect();
	names.sort_unstable();
	println!("internal: {}", names.join(" "));
	match hlp.topics() {
		Ok(topics) => {
			println!("topics:   {}\n", topics.len());
			for topic in &topics {
				println!("=== [{}] {} (offset {}) ===", topic.number, topic.title, topic.offset);
				println!("{}", topic.text());
				for block in &topic.blocks {
					if let Block::Table(t) = block {
						println!("  <table {} rows>", t.rows.len());
						for row in &t.rows {
							let cells: Vec<String> = row.cells.iter().map(libhlp::Cell::text).collect();
							println!("    | {}", cells.join(" | "));
						}
					}
				}
				for paragraph in topic.paragraphs() {
					for run in &paragraph.runs {
						if let Run::Image(number) = run {
							println!("  <image {number:?}>");
						}
						if let Run::Text { text, format, .. } = run
							&& !format.is_plain()
						{
							let mut on = String::new();
							if format.bold {
								on.push('b');
							}
							if format.italic {
								on.push('i');
							}
							if format.underline {
								on.push('u');
							}
							println!("  [{on}] {text:?}");
						}
					}
				}
				let links: Vec<String> = topic
					.links()
					.map(|l| {
						hlp.topic_offset(l).map_or_else(
							|| format!("{:?}", l.target),
							|offset| format!("{:?} -> topic at {offset}", l.target),
						)
					})
					.collect();
				if !links.is_empty() {
					println!("  links: {}", links.join(", "));
				}
				println!();
			}
		}
		Err(e) => eprintln!("topics: {e}"),
	}
}
