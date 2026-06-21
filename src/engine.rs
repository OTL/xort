//! The sort engine: orchestrates reading, the chosen execution path, and output.

use crate::compare::{compare_key, full_compare, KeyOpts};
use crate::config::Config;
use crate::input::{read_all, split_lines};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::time::Instant;

/// Outcome of a run: the process exit code plus optional statistics.
pub struct Outcome {
    pub exit_code: i32,
    pub stats: Option<Stats>,
}

pub struct Stats {
    pub lines_in: usize,
    pub lines_out: usize,
    pub duplicates_removed: usize,
    pub elapsed_secs: f64,
}

/// Execute the sort job described by `cfg`.
pub fn run(cfg: &Config) -> io::Result<Outcome> {
    if let Some(n) = cfg.parallel {
        // Best-effort: silently ignored if a global pool already exists.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build_global();
    }

    let start = Instant::now();
    let terminator = cfg.terminator();
    let data = read_all(&cfg.files, terminator)?;
    let mut lines = split_lines(&data, terminator);
    let lines_in = lines.len();
    let opts = cfg.key_opts();

    if cfg.check {
        let code = check_sorted(&lines, cfg, &opts);
        return Ok(Outcome {
            exit_code: code,
            stats: None,
        });
    }

    // Top-N without unique can short-circuit the full sort. With unique we sort
    // fully, dedup, then truncate so "--top N" means "the first N distinct keys".
    let fused_top = cfg.top.filter(|_| !cfg.unique);
    match fused_top {
        Some(n) => select_top(&mut lines, n, cfg, &opts),
        None => sort_all(&mut lines, cfg, &opts),
    }

    let mut duplicates_removed = 0;
    if cfg.unique {
        let before = lines.len();
        lines.dedup_by(|a, b| compare_key(a, b, &opts) == Ordering::Equal);
        duplicates_removed = before - lines.len();
        if let Some(n) = cfg.top {
            lines.truncate(n);
        }
    }

    write_output(&lines, cfg, terminator)?;

    let stats = cfg.stats.then(|| Stats {
        lines_in,
        lines_out: lines.len(),
        duplicates_removed,
        elapsed_secs: start.elapsed().as_secs_f64(),
    });
    Ok(Outcome {
        exit_code: 0,
        stats,
    })
}

fn sort_all(lines: &mut [&[u8]], cfg: &Config, opts: &KeyOpts) {
    // `-u` keeps the first line (in input order) of each equal-key run, so GNU
    // disables the whole-line last-resort comparison and effectively sorts
    // stably by key. We mirror that: a stable sort with last-resort suppressed.
    let stable = cfg.stable || cfg.unique;
    if stable {
        lines.par_sort_by(|a, b| full_compare(a, b, opts, cfg.reverse, true));
    } else {
        lines.par_sort_unstable_by(|a, b| full_compare(a, b, opts, cfg.reverse, false));
    }
}

/// Fused top-N: partition out the N smallest (per the sort order) in O(n) with
/// `select_nth_unstable`, then order just those N — avoiding the full sort that
/// `sort | head -N` performs.
fn select_top(lines: &mut Vec<&[u8]>, n: usize, cfg: &Config, opts: &KeyOpts) {
    let n = n.min(lines.len());
    if n == 0 {
        lines.clear();
        return;
    }
    if n < lines.len() {
        lines.select_nth_unstable_by(n - 1, |a, b| full_compare(a, b, opts, cfg.reverse, false));
        lines.truncate(n);
    }
    sort_all(lines, cfg, opts);
}

fn check_sorted(lines: &[&[u8]], cfg: &Config, opts: &KeyOpts) -> i32 {
    for (i, win) in lines.windows(2).enumerate() {
        let (prev, cur) = (win[0], win[1]);
        let mut ord = compare_key(prev, cur, opts);
        if cfg.reverse {
            ord = ord.reverse();
        }
        let disordered = ord == Ordering::Greater || (cfg.unique && ord == Ordering::Equal);
        if disordered {
            // Best-effort GNU-style message; rich diagnostics arrive in milestone 5.
            eprintln!(
                "fsort: -:{}: disorder: {}",
                i + 2, // the offending line is the second of the window (1-based)
                String::from_utf8_lossy(cur)
            );
            return 1;
        }
    }
    0
}

fn write_output(lines: &[&[u8]], cfg: &Config, terminator: u8) -> io::Result<()> {
    match &cfg.output {
        Some(path) => {
            let f = File::create(path)?;
            write_lines(BufWriter::new(f), lines, terminator)
        }
        None => {
            let stdout = io::stdout();
            write_lines(BufWriter::new(stdout.lock()), lines, terminator)
        }
    }
}

fn write_lines<W: Write>(mut w: W, lines: &[&[u8]], terminator: u8) -> io::Result<()> {
    for line in lines {
        w.write_all(line)?;
        w.write_all(std::slice::from_ref(&terminator))?;
    }
    w.flush()
}
