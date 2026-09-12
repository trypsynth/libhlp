//! Print what libhlp reads out of a help file.
//!
//! ```sh
//! cargo run --example dump -- manual.hlp
//! ```

use std::{env, process};

use libhlp::HlpFile;

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
