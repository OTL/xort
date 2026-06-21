//! External merge sort: for inputs that should not be held fully in memory.
//!
//! Activated by `-S SIZE`. The input is streamed in `SIZE`-byte chunks; each
//! chunk is sorted in memory and spilled to a temp file as terminated lines;
//! the sorted runs are then k-way merged to the output. Temp files are cleaned
//! up via `tempfile`'s RAII (including on error unwind).

use crate::config::Config;
use crate::key::Sorter;
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

/// A streaming line reader over multiple inputs, yielding owned lines.
struct LineSource {
    readers: Vec<Box<dyn Read>>,
    idx: usize,
    buf: Vec<u8>,
    pos: usize,
    filled: usize,
    terminator: u8,
    carry: Vec<u8>,
}

impl LineSource {
    fn new(cfg: &Config, terminator: u8) -> io::Result<Self> {
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
        Ok(LineSource {
            readers,
            idx: 0,
            buf: vec![0u8; 1 << 16],
            pos: 0,
            filled: 0,
            terminator,
            carry: Vec::new(),
        })
    }

    /// Return the next complete line (without terminator) as an owned Vec.
    fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            // Scan the live buffer region for a terminator.
            if self.pos < self.filled {
                if let Some(rel) = memchr::memchr(self.terminator, &self.buf[self.pos..self.filled])
                {
                    let end = self.pos + rel;
                    let line = if self.carry.is_empty() {
                        self.buf[self.pos..end].to_vec()
                    } else {
                        let mut v = std::mem::take(&mut self.carry);
                        v.extend_from_slice(&self.buf[self.pos..end]);
                        v
                    };
                    self.pos = end + 1;
                    return Ok(Some(line));
                } else {
                    self.carry
                        .extend_from_slice(&self.buf[self.pos..self.filled]);
                    self.pos = self.filled;
                }
            }
            // Refill.
            if self.idx >= self.readers.len() {
                if !self.carry.is_empty() {
                    return Ok(Some(std::mem::take(&mut self.carry)));
                }
                return Ok(None);
            }
            self.filled = self.readers[self.idx].read(&mut self.buf)?;
            self.pos = 0;
            if self.filled == 0 {
                // End of this reader; flush any carry as a final line.
                self.idx += 1;
                if !self.carry.is_empty() && self.idx >= self.readers.len() {
                    return Ok(Some(std::mem::take(&mut self.carry)));
                }
            }
        }
    }
}

/// Run the external sort, writing sorted output. Returns `(lines, chunks)`
/// where `chunks` is the number of sorted runs spilled to temp files (1 means
/// everything fit in a single in-memory chunk; > 1 means the input genuinely
/// spilled and was k-way merged).
pub fn run_external(
    cfg: &Config,
    sorter: &Sorter,
    budget: usize,
    terminator: u8,
) -> io::Result<(usize, usize)> {
    let mut src = LineSource::new(cfg, terminator)?;
    let mut runs: Vec<NamedTempFile> = Vec::new();
    let mut chunk: Vec<Vec<u8>> = Vec::new();
    let mut chunk_bytes = 0usize;
    let mut total = 0usize;

    let temp_dir = cfg.temp_dirs.first().cloned();

    while let Some(line) = src.next_line()? {
        chunk_bytes += line.len() + 1;
        chunk.push(line);
        total += 1;
        if chunk_bytes >= budget {
            spill(&mut chunk, sorter, cfg, terminator, &temp_dir, &mut runs)?;
            chunk_bytes = 0;
        }
    }
    if !chunk.is_empty() {
        spill(&mut chunk, sorter, cfg, terminator, &temp_dir, &mut runs)?;
    }

    // Merge the runs to the output.
    let out: Box<dyn Write> = match &cfg.output {
        Some(p) => Box::new(BufWriter::new(File::create(p)?)),
        None => Box::new(BufWriter::new(io::stdout().lock())),
    };
    let chunks = runs.len();
    merge_runs(runs, sorter, cfg, terminator, out)?;
    Ok((total, chunks))
}

fn spill(
    chunk: &mut Vec<Vec<u8>>,
    sorter: &Sorter,
    cfg: &Config,
    terminator: u8,
    temp_dir: &Option<std::path::PathBuf>,
    runs: &mut Vec<NamedTempFile>,
) -> io::Result<()> {
    if cfg.stable || cfg.unique {
        chunk.sort_by(|a, b| sorter.compare(a, b));
    } else {
        chunk.sort_unstable_by(|a, b| sorter.compare(a, b));
    }
    let mut tf = match temp_dir {
        Some(d) => NamedTempFile::new_in(d)?,
        None => NamedTempFile::new()?,
    };
    {
        let mut w = BufWriter::new(tf.as_file_mut());
        for line in chunk.iter() {
            w.write_all(line)?;
            w.write_all(std::slice::from_ref(&terminator))?;
        }
        w.flush()?;
    }
    runs.push(tf);
    chunk.clear();
    Ok(())
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
            reader: BufReader::new(file),
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
