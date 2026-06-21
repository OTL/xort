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

/// Concatenate all inputs (files and/or stdin) into one reader.
fn open_input(cfg: &Config) -> io::Result<Box<dyn Read>> {
    if cfg.files.is_empty() {
        return Ok(Box::new(io::stdin()));
    }
    let mut chained: Option<Box<dyn Read>> = None;
    for p in &cfg.files {
        let r: Box<dyn Read> = if p.as_os_str() == "-" {
            Box::new(io::stdin())
        } else {
            Box::new(
                File::open(p)
                    .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", p.display(), e)))?,
            )
        };
        chained = Some(match chained {
            None => r,
            Some(prev) => Box::new(prev.chain(r)),
        });
    }
    Ok(chained.unwrap_or_else(|| Box::new(io::empty())))
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
    // Hit the budget mid-stream: split after the last complete line.
    loop {
        if let Some(p) = memchr::memrchr(terminator, block) {
            *carry = block.split_off(p + 1);
            return Ok(false);
        }
        // No terminator yet (a single line exceeds the budget) — keep reading.
        let n = r.read(&mut tmp)?;
        if n == 0 {
            return Ok(true);
        }
        block.extend_from_slice(&tmp[..n]);
    }
}

/// Split a buffer of terminated lines into borrowed slices (terminators
/// stripped). A final unterminated line is included.
fn split_block(block: &[u8], terminator: u8) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for i in memchr::memchr_iter(terminator, block) {
        lines.push(&block[start..i]);
        start = i + 1;
    }
    if start < block.len() {
        lines.push(&block[start..]);
    }
    lines
}

/// Run the external sort, writing sorted output. Returns `(lines, chunks)`
/// where `chunks` is the number of sorted runs (1 means everything fit in a
/// single chunk and was written directly; > 1 means the input genuinely
/// spilled to temp files and was k-way merged).
///
/// Each chunk is read as one block of zero-copy line slices and sorted in
/// parallel (rayon); only when the input exceeds the budget do we spill and
/// merge — a single chunk streams straight to the output with no temp files.
pub fn run_external(
    cfg: &Config,
    sorter: &Sorter,
    budget: usize,
    terminator: u8,
) -> io::Result<(usize, usize)> {
    let mut reader = open_input(cfg)?;
    let temp_dir = cfg.temp_dirs.first().cloned();
    let stable = cfg.stable || cfg.unique;

    let mut out: Box<dyn Write> = match &cfg.output {
        Some(p) => Box::new(BufWriter::with_capacity(IO_BUF, File::create(p)?)),
        None => Box::new(BufWriter::with_capacity(IO_BUF, io::stdout().lock())),
    };

    let mut runs: Vec<NamedTempFile> = Vec::new();
    let mut total = 0usize;
    let mut carry: Vec<u8> = Vec::new();
    let mut block: Vec<u8> = Vec::with_capacity(budget.min(1 << 26) + IO_BUF);

    loop {
        let eof = read_block(&mut *reader, budget, terminator, &mut carry, &mut block)?;
        if block.is_empty() {
            break;
        }
        let mut lines = split_block(&block, terminator);
        total += lines.len();
        if stable {
            lines.par_sort_by(|a, b| sorter.compare(a, b));
        } else {
            lines.par_sort_unstable_by(|a, b| sorter.compare(a, b));
        }

        // Single chunk that reached EOF on the first read: write directly,
        // skipping the temp-file round trip entirely.
        if eof && runs.is_empty() {
            write_lines(&mut out, &lines, terminator)?;
            out.flush()?;
            return Ok((total, 1));
        }

        runs.push(spill(&lines, terminator, &temp_dir)?);
        if eof {
            break;
        }
    }

    let chunks = runs.len();
    merge_runs(runs, sorter, cfg, terminator, out)?;
    Ok((total, chunks))
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
) -> io::Result<()> {
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
    let mut last: Option<Vec<u8>> = None;
    while let Some(mut cur) = heap.pop() {
        let dup = cfg.unique
            && last
                .as_ref()
                .is_some_and(|l| sorter.key_equal(l, &cur.head));
        if !dup {
            out.write_all(&cur.head)?;
            out.write_all(std::slice::from_ref(&terminator))?;
            if cfg.unique {
                last = Some(cur.head.clone());
            }
        }
        if cur.advance()? {
            heap.push(cur);
        }
    }
    out.flush()?;
    // `runs` is dropped here, deleting temp files.
    drop(runs);
    Ok(())
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
}
