//! The sort engine: orchestrates reading, the chosen execution path, and output.

use crate::compare::{compare_key, full_compare, KeyOpts, NumericKey};
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
    let lines = split_lines(&data, terminator);
    let lines_in = lines.len();
    let opts = cfg.key_opts();

    if cfg.check {
        let code = check_sorted(&lines, cfg, &opts);
        return Ok(Outcome {
            exit_code: code,
            stats: None,
        });
    }

    let (ordered, duplicates_removed) = if opts.numeric {
        numeric_order(lines, cfg)
    } else {
        byte_order(lines, cfg, &opts)
    };

    write_output(&ordered, cfg, terminator)?;

    let stats = cfg.stats.then(|| Stats {
        lines_in,
        lines_out: ordered.len(),
        duplicates_removed,
        elapsed_secs: start.elapsed().as_secs_f64(),
    });
    Ok(Outcome {
        exit_code: 0,
        stats,
    })
}

/// `-u` keeps the first line (in input order) of each equal-key run, so GNU
/// disables the whole-line last-resort comparison and effectively sorts stably
/// by key. We mirror that: stable + last-resort suppressed when `-u`.
#[inline]
fn stable_for(cfg: &Config) -> bool {
    cfg.stable || cfg.unique
}

/// Plain/fold byte path (no `-n`).
fn byte_order<'a>(
    mut lines: Vec<&'a [u8]>,
    cfg: &Config,
    opts: &KeyOpts,
) -> (Vec<&'a [u8]>, usize) {
    // For `-u`, a stable sort only matters when a transform can equate lines
    // that differ in bytes (`-f` fold, `-b` ignore-blanks). With a plain byte
    // key, equal keys are byte-identical, so the faster unstable sort is safe.
    let stable = cfg.stable || (cfg.unique && (opts.fold || opts.ignore_blanks));
    let cmp = |a: &&[u8], b: &&[u8]| full_compare(a, b, opts, cfg.reverse, stable);

    let fused_top = cfg.top.filter(|_| !cfg.unique);
    match fused_top {
        Some(n) => {
            let n = n.min(lines.len());
            if n == 0 {
                return (Vec::new(), 0);
            }
            if n < lines.len() {
                lines.select_nth_unstable_by(n - 1, |a, b| {
                    full_compare(a, b, opts, cfg.reverse, false)
                });
                lines.truncate(n);
            }
            lines.par_sort_unstable_by(cmp);
        }
        None if stable => lines.par_sort_by(cmp),
        None => lines.par_sort_unstable_by(cmp),
    }

    let mut dups = 0;
    if cfg.unique {
        let before = lines.len();
        lines.dedup_by(|a, b| compare_key(a, b, opts) == Ordering::Equal);
        dups = before - lines.len();
        if let Some(n) = cfg.top {
            lines.truncate(n);
        }
    }
    (lines, dups)
}

/// Numeric path with decorate-sort-undecorate: parse each line's leading number
/// once (in parallel), sort the cheap precomputed keys, then drop them.
fn numeric_order<'a>(lines: Vec<&'a [u8]>, cfg: &Config) -> (Vec<&'a [u8]>, usize) {
    let stable = stable_for(cfg);
    let mut dec: Vec<(NumericKey<'a>, &'a [u8])> = lines
        .into_par_iter()
        .map(|l| (NumericKey::parse(l), l))
        .collect();

    let cmp = |a: &(NumericKey, &[u8]), b: &(NumericKey, &[u8])| {
        let mut o = a.0.cmp(&b.0);
        if o == Ordering::Equal && !stable {
            o = a.1.cmp(b.1); // whole-line last resort
        }
        if cfg.reverse {
            o.reverse()
        } else {
            o
        }
    };

    let fused_top = cfg.top.filter(|_| !cfg.unique);
    match fused_top {
        Some(n) => {
            let n = n.min(dec.len());
            if n == 0 {
                return (Vec::new(), 0);
            }
            if n < dec.len() {
                dec.select_nth_unstable_by(n - 1, cmp);
                dec.truncate(n);
            }
            dec.par_sort_unstable_by(cmp);
        }
        None if stable => dec.par_sort_by(cmp),
        None => dec.par_sort_unstable_by(cmp),
    }

    let mut dups = 0;
    if cfg.unique {
        let before = dec.len();
        dec.dedup_by(|a, b| a.0.cmp(&b.0) == Ordering::Equal);
        dups = before - dec.len();
        if let Some(n) = cfg.top {
            dec.truncate(n);
        }
    }

    let out = dec.into_iter().map(|(_, l)| l).collect();
    (out, dups)
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
