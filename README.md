# libhlp

Pure-Rust reader for WinHelp (`.hlp`) files, including LZ77 and both phrase compression schemes. No unsafe code, no C dependencies.

## Installation

```sh
cargo add libhlp
```

Requires Rust 1.87 or newer.

## Usage

```rust
use libhlp::HlpFile;

fn main() -> libhlp::Result<()> {
    let hlp = HlpFile::open("manual.hlp")?;
    println!("{}", hlp.title().unwrap_or("untitled"));
    for topic in hlp.topics()? {
        println!("== {} ==", topic.title);
        println!("{}", topic.text());
    }
    Ok(())
}
```

There is a worked example in `examples/dump.rs`:

```sh
cargo run --example dump -- manual.hlp
```

### Runs and links

`Topic::text` flattens a topic. To keep the structure, walk the paragraphs yourself:

```rust
use libhlp::{Run, Target};

for paragraph in &topic.paragraphs {
    for run in &paragraph.runs {
        match run {
            Run::Text { text, link: Some(link) } => match &link.target {
                Target::Context(_) => match hlp.topic_offset(link) {
                    Some(offset) => println!("{text} -> topic at {offset}"),
                    None => println!("{text} -> unresolved"),
                },
                Target::External { file, .. } => println!("{text} -> {file}"),
                other => println!("{text} -> {other:?}"),
            },
            Run::Text { text, .. } => print!("{text}"),
            Run::Tab => print!("\t"),
            Run::LineBreak => println!(),
            Run::Image => print!("[image]"),
        }
    }
}
```

WinHelp does not store a link's destination as a topic offset. It stores the hash of the target's context string, so `HlpFile::topic_offset` looks that up in the file's `|CONTEXT` index. `libhlp::hash` computes the same value from a context string, for finding a topic whose name you already know.

### Internal files

A `.hlp` is an archive with its own directory. `read_file` hands back the raw bytes of anything this crate does not interpret, such as the bitmaps in `|bmN`:

```rust
for name in hlp.file_names() {
    println!("{name}");
}
let bitmap = hlp.read_file("|bm0")?;
```

## What is supported

| Format | Version | Status |
| --- | --- | --- |
| WinHelp 3.1 (`HC31`) | 21 | Read, with `\|Phrases` compression |
| Media view (`WMVC`/`MMVC`) | 27 | Read |
| Windows 95 (`HCW 4.00`) | 33 | Read, with Hall (`\|PhrIndex`/`\|PhrImage`) compression |
| WinHelp 3.0 (`HC30`) | 15 | Rejected with `HlpError::UnsupportedVersion` |

WinHelp 3.0 uses a different topic layout and is not read yet.

Files from Borland's help compiler and MS-DOS QuickHelp share the `.hlp` extension but are unrelated formats. They are rejected with `HlpError::BadMagic`.

What is read: topic text, topic titles, paragraph and line structure, tab stops, hotspots and their targets, picture positions, and the file's own metadata. What is not: font and colour formatting, table geometry, keyword indexes, and the contents of embedded pictures. The raw bytes for all of it are still reachable through `read_file`.

## Text encoding

WinHelp predates Unicode and stores text in a single-byte Windows code page. This crate decodes as Windows-1252, which is right for Western European files. `HlpFile::charset` reports what the file claims, so a caller that needs another code page can re-decode the bytes itself.

## Robustness

Help files are untrusted input. Every offset and length is bounds checked before use, and a damaged topic ends the walk rather than causing a panic, so the topics already read are still returned.

## Credits

The format is not documented by Microsoft. This crate is written against [`helpfile.txt`](https://github.com/pmachapman/helpdeco/blob/master/helpfile.txt), Manfred Winterhoff's reverse-engineered description of the format, and against [helpdeco](https://github.com/pmachapman/helpdeco) itself where the two disagree.

## License

MIT
