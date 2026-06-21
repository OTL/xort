//! External merge sort: for inputs that should not be held fully in memory.
//!
//! Activated by `-S SIZE`. The input is streamed in `SIZE`-byte chunks; each
//! chunk is read as one block of zero-copy line slices and sorted in parallel
//! (rayon). A single chunk streams straight to the output; multiple chunks are
//! spilled to temp files and k-way merged. Temp files are cleaned up via
//! `tempfile`'s RAII (including on error unwind).

use crate::config::Config;
use crate::key::Sorter;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use tempfile::NamedTempFile;

/// Parse a GNU-style buffer size: bare bytes, or a K/M/G/T/P suffix (1024-based).
/// A trailing `b` means bytes; `%` (percentage of RAM) is not supported.
pub fn parse_size(s: &str) -> Result<usize, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty buffer size".into());
    }
    let bytes = s.as_bytes();
    let (num, mult): (&str, u64) = match bytes[bytes.len() - 1] {
        b'b' | b'B' => (&s[..s.len() - 1], 1),
        b'k' | b'K' => (&s[..s.len() - 1], 1 << 10),
        b'm' | b'M' => (&s[..s.len() - 1], 1 << 20),
        b'g' | b'G' => (&s[..s.len() - 1], 1 << 30),
        b't' | b'T' => (&s[..s.len() - 1], 1u64 << 40),
        b'p' | b'P' => (&s[..s.len() - 1], 1u64 << 50),
        _ => (s, 1),
    };
    let n: u64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid buffer size '{s}'"))?;
    Ok((n.saturating_mul(mult)).max(1) as usize)
}

/// Read buffers sized for throughput rather than latency.
const IO_BUF: usize = 1 << 18; // 256 KiB

/// A `Read` over all inputs in order that injects a single terminator between
/// files which do not already end with one — matching `input::read_all`, so the
/// external path never glues the last line of one file to the next file's first
/// line.
struct MultiInput {
    readers: Vec<Box<dyn Read>>,
    idx: usize,
    terminator: u8,
    last_byte: Option<u8>, // last data byte read from the current file
    pending_sep: bool,
}

impl Read for MultiInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pending_sep {
                self.pending_sep = false;
                buf[0] = self.terminator;
                return Ok(1);
            }
            if self.idx >= self.readers.len() {
                return Ok(0);
            }
            let n = self.readers[self.idx].read(buf)?;
            if n == 0 {
                // Current file ended; if it emitted bytes not ending in the
                // terminator and another file follows, inject a separator.
                self.idx += 1;
                if self.idx < self.readers.len()
                    && self.last_byte.is_some_and(|b| b != self.terminator)
                {
                    self.pending_sep = true;
                }
                self.last_byte = None;
                continue;
            }
            self.last_byte = Some(buf[n - 1]);
            return Ok(n);
        }
    }
}

/// Open all inputs (files and/or stdin) as one separator-inserting reader.
fn open_input(cfg: &Config, terminator: u8) -> io::Result<MultiInput> {
    let mut readers: Vec<Box<dyn Read>> = Vec::new();
    if cfg.files.is_empty() {
        readers.push(Box::new(io::stdin()));
    } else {
        for p in &cfg.files {
            if p.as_os_str() == "-" {
                readers.push(Box::new(io::stdin()));
            } else {
                readers.push(Box::new(File::open(p).map_err(|e| {
                    io::Error::new(e.kind(), format!("{}: {}", p.display(), e))
                })?));
            }
        }
    }
    Ok(MultiInput {
        readers,
        idx: 0,
        terminator,
        last_byte: None,
        pending_sep: false,
    })
}

/// Read the next chunk of *complete* lines into `block`, bounded by `budget`
/// bytes. Any trailing partial line is preserved in `carry` for the next call.
/// Returns whether EOF was reached.
fn read_block(
    r: &mut dyn Read,
    budget: usize,
    terminator: u8,
    carry: &mut Vec<u8>,
    block: &mut Vec<u8>,
) -> io::Result<bool> {
    block.clear();
    block.append(carry); // start with the leftover partial line, if any
    let mut tmp = [0u8; IO_BUF];
    while block.len() < budget {
        let n = r.read(&mut tmp)?;
        if n == 0 {
            return Ok(true); // EOF: everything in `block` is this final chunk
        }
        block.extend_from_slice(&tmp[..n]);
    }
    // Hit the budget mid-stream: split after the last complete line. `scanned`
    // tracks the prefix already known to contain no terminator, so an
    // oversized line (no terminator within budget) is scanned once overall
    // rather than re-scanning the whole growing block on each read (O(n) not
    // O(n^2)).
    let mut scanned = 0;
    loop {
        if let Some(p) = memchr::memrchr(terminator, &block[scanned..]) {
            *carry = block.split_off(scanned + p + 1);
            return Ok(false);
        }
        scanned = block.len();
        // No terminator yet (a single line exceeds the budget) — keep reading.
        let n = r.read(&mut tmp)?;
        if n == 0 {
            return Ok(true);
        }
        block.extend_from_slice(&tmp[..n]);
    }
}

/// Run the external sort, writing sorted output. Returns `(lines_in, lines_out,
/// chunks)`: `lines_in` is the number of records read, `lines_out` the number
/// written (smaller under `-u`), and `chunks` the number of sorted runs (1 means
/// everything fit in a single chunk and was written directly; > 1 means the
/// input genuinely spilled to temp files and was k-way merged).
///
/// Each chunk is read as one block of zero-copy line slices and sorted in
/// parallel (rayon); only when the input exceeds the budget do we spill and
/// merge — a single chunk streams straight to the output with no temp files.
pub fn run_external(
    cfg: &Config,
    sorter: &Sorter,
    budget: usize,
    terminator: u8,
) -> io::Result<(usize, usize, usize)> {
    let mut reader = open_input(cfg, terminator)?;
    let temp_dir = cfg.temp_dirs.first().cloned();
    let stable = cfg.stable || cfg.unique;

    // The output file is created lazily, only after the input has been fully
    // consumed (either into a single in-memory chunk or spilled to temp runs).
    // Opening it eagerly would truncate it first, destroying the input when
    // `-o FILE` names a file that is also an input (`xort -S ... -o f f`).
    let mut runs: Vec<NamedTempFile> = Vec::new();
    let mut total = 0usize;
    let mut carry: Vec<u8> = Vec::new();
    let mut block: Vec<u8> = Vec::with_capacity(budget.min(1 << 26) + IO_BUF);

    loop {
        let eof = read_block(&mut reader, budget, terminator, &mut carry, &mut block)?;
        if block.is_empty() {
            break;
        }
        let mut lines = crate::input::split_lines(&block, terminator);
        total += lines.len();
        if stable {
            lines.par_sort_by(|a, b| sorter.compare(a, b));
        } else {
            lines.par_sort_unstable_by(|a, b| sorter.compare(a, b));
        }

        // Single chunk that reached EOF on the first read: dedup for -u, then
        // write directly, skipping the temp-file round trip entirely.
        if eof && runs.is_empty() {
            if cfg.unique {
                lines.dedup_by(|a, b| sorter.key_equal(a, b));
            }
            // Input fully read; safe to truncate the output even if it aliases
            // an input file.
            let mut out = open_output(cfg)?;
            write_lines(&mut out, &lines, terminator)?;
            out.flush()?;
            return Ok((total, lines.len(), 1));
        }

        runs.push(spill(&lines, terminator, &temp_dir)?);
        if eof {
            break;
        }
    }

    let chunks = runs.len();
    // All input has been read and spilled to temp runs, so creating the output
    // (which may alias an input file) is now safe.
    let out = open_output(cfg)?;
    let lines_out = merge_runs(runs, sorter, cfg, terminator, out)?;
    Ok((total, lines_out, chunks))
}

/// Open the destination writer: the `-o FILE` target (created/truncated) or
/// stdout. Created lazily by `run_external` once all input is consumed.
fn open_output(cfg: &Config) -> io::Result<Box<dyn Write>> {
    Ok(match &cfg.output {
        Some(p) => Box::new(BufWriter::with_capacity(
            IO_BUF,
            File::create(p)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", p.display(), e)))?,
        )),
        None => Box::new(BufWriter::with_capacity(IO_BUF, io::stdout().lock())),
    })
}

fn write_lines(w: &mut dyn Write, lines: &[&[u8]], terminator: u8) -> io::Result<()> {
    for line in lines {
        w.write_all(line)?;
        w.write_all(std::slice::from_ref(&terminator))?;
    }
    Ok(())
}

/// Write one sorted run to a temp file and hand back its handle.
fn spill(
    lines: &[&[u8]],
    terminator: u8,
    temp_dir: &Option<std::path::PathBuf>,
) -> io::Result<NamedTempFile> {
    let mut tf = match temp_dir {
        Some(d) => NamedTempFile::new_in(d)?,
        None => NamedTempFile::new()?,
    };
    {
        let mut w = BufWriter::with_capacity(IO_BUF, tf.as_file_mut());
        write_lines(&mut w, lines, terminator)?;
        w.flush()?;
    }
    Ok(tf)
}

/// A run cursor in the merge heap, ordered by its current head line.
struct Cursor<'a> {
    reader: BufReader<File>,
    head: Vec<u8>,
    sorter: &'a Sorter,
    terminator: u8,
}

impl Cursor<'_> {
    fn advance(&mut self) -> io::Result<bool> {
        self.head.clear();
        let n = self.reader.read_until(self.terminator, &mut self.head)?;
        if n == 0 {
            return Ok(false);
        }
        if self.head.last() == Some(&self.terminator) {
            self.head.pop();
        }
        Ok(true)
    }
}

impl Ord for Cursor<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse so BinaryHeap (max-heap) yields the smallest line first.
        self.sorter.compare(&self.head, &other.head).reverse()
    }
}
impl PartialOrd for Cursor<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Cursor<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.sorter.compare(&self.head, &other.head) == Ordering::Equal
    }
}
impl Eq for Cursor<'_> {}

fn merge_runs(
    runs: Vec<NamedTempFile>,
    sorter: &Sorter,
    cfg: &Config,
    terminator: u8,
    mut out: Box<dyn Write>,
) -> io::Result<usize> {
    let mut heap = BinaryHeap::new();
    for tf in &runs {
        let file = tf.reopen()?;
        let mut cur = Cursor {
            reader: BufReader::with_capacity(IO_BUF, file),
            head: Vec::new(),
            sorter,
            terminator,
        };
        if cur.advance()? {
            heap.push(cur);
        }
    }
    let mut written = 0usize;
    let mut last: Option<Vec<u8>> = None;
    while let Some(mut cur) = heap.pop() {
        let dup = cfg.unique
            && last
                .as_ref()
                .is_some_and(|l| sorter.key_equal(l, &cur.head));
        if !dup {
            out.write_all(&cur.head)?;
            out.write_all(std::slice::from_ref(&terminator))?;
            written += 1;
            if cfg.unique {
                // Reuse one buffer across rows instead of allocating per line.
                let mut prev = last.take().unwrap_or_default();
                prev.clear();
                prev.extend_from_slice(&cur.head);
                last = Some(prev);
            }
        }
        if cur.advance()? {
            heap.push(cur);
        }
    }
    out.flush()?;
    // `runs` is dropped here, deleting temp files.
    drop(runs);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("500").unwrap(), 500);
        assert_eq!(parse_size("64K").unwrap(), 64 * 1024);
        assert_eq!(parse_size("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn sizes_all_suffixes_and_edges() {
        // A bare `b`/`B` suffix means bytes (multiplier 1).
        assert_eq!(parse_size("42b").unwrap(), 42);
        assert_eq!(parse_size("7B").unwrap(), 7);
        // The large suffixes T and P. Compute in u64 (not usize) so the shift
        // does not overflow at compile time on 32-bit targets.
        assert_eq!(parse_size("1T").unwrap(), (1u64 << 40) as usize);
        assert_eq!(parse_size("1P").unwrap(), (1u64 << 50) as usize);
        // Case-insensitive units and surrounding whitespace.
        assert_eq!(parse_size("3k").unwrap(), 3 * 1024);
        assert_eq!(parse_size("  8m  ").unwrap(), 8 * 1024 * 1024);
        // A size of zero is clamped up to at least one byte.
        assert_eq!(parse_size("0").unwrap(), 1);
        // An empty string is rejected.
        assert!(parse_size("").is_err());
        assert!(parse_size("   ").is_err());
    }
}
