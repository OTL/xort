//! The sort engine: orchestrates reading, the chosen execution path, and output.

use crate::compare::{compare_key, full_compare, KeyOpts, NumericKey};
use crate::config::Config;
use crate::input::{read_all, read_each, split_lines};
use crate::key::Sorter;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::time::Instant;

/// Outcome of a run: the process exit code plus optional statistics.
pub struct Outcome {
    /// Process exit code (0 success, 1 `-c` disorder, 2 error).
    pub exit_code: i32,
    /// Summary statistics, present when `--stats` was requested.
    pub stats: Option<Stats>,
}

/// Summary statistics for a run, emitted to stderr under `--stats`.
pub struct Stats {
    /// Number of input lines (records) read.
    pub lines_in: usize,
    /// Number of lines (records/groups) written.
    pub lines_out: usize,
    /// Lines removed by `-u`/grouping.
    pub duplicates_removed: usize,
    /// Wall-clock time elapsed.
    pub elapsed_secs: f64,
}

fn invalid(e: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e)
}

/// Execute the sort job described by `cfg`.
pub fn run(cfg: &Config) -> io::Result<Outcome> {
    if let Some(n) = cfg.parallel {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build_global();
    }
    let start = Instant::now();
    let terminator = cfg.terminator();

    // Structured formats (CSV/TSV/JSON/JSONL) have their own path.
    if cfg.format != crate::config::Format::Text {
        return crate::format::run_structured(cfg, start);
    }

    // External merge sort: gated behind -S, and only for the plain/unique sort
    // (not --top/--count/--header/--merge, which use the in-memory paths).
    if let Some(size) = &cfg.buffer_size {
        if !cfg.merge && !cfg.check && cfg.top.is_none() && !cfg.count && !cfg.header {
            let budget = crate::external::parse_size(size).map_err(invalid)?;
            let sorter = cfg.build_sorter().map_err(invalid)?;
            let n = crate::external::run_external(cfg, &sorter, budget, terminator)?;
            let stats = cfg.stats.then(|| Stats {
                lines_in: n,
                lines_out: n,
                duplicates_removed: 0,
                elapsed_secs: start.elapsed().as_secs_f64(),
            });
            return Ok(Outcome {
                exit_code: 0,
                stats,
            });
        }
    }

    // Merge: inputs are already sorted; k-way merge them.
    if cfg.merge {
        let sorter = cfg.build_sorter().map_err(invalid)?;
        let buffers = read_each(&cfg.files, terminator)?;
        let per_file: Vec<Vec<&[u8]>> =
            buffers.iter().map(|b| split_lines(b, terminator)).collect();
        let lines_in: usize = per_file.iter().map(|v| v.len()).sum();
        let (ordered, dups) = merge_sorted(per_file, cfg, &sorter);
        write_output(&ordered, cfg, terminator)?;
        let stats = cfg.stats.then(|| Stats {
            lines_in,
            lines_out: ordered.len(),
            duplicates_removed: dups,
            elapsed_secs: start.elapsed().as_secs_f64(),
        });
        return Ok(Outcome {
            exit_code: 0,
            stats,
        });
    }

    let data = read_all(&cfg.files, terminator)?;
    let mut lines = split_lines(&data, terminator);

    // --header: peel off the first line, pin it on output.
    let header = if cfg.header && !lines.is_empty() {
        Some(lines.remove(0))
    } else {
        None
    };
    let lines_in = lines.len();

    if cfg.check {
        let sorter = cfg.build_sorter().map_err(invalid)?;
        let code = check_sorted(&lines, cfg, &sorter);
        return Ok(Outcome {
            exit_code: code,
            stats: None,
        });
    }

    // Fused dedup + count (like `sort | uniq -c`): one line per equal-key group,
    // prefixed with its occurrence count.
    if cfg.count {
        let sorter = cfg.build_sorter().map_err(invalid)?;
        let groups = grouped_counts(lines, &sorter, cfg);
        let lines_out = groups.len();
        write_counts(&groups, header, cfg, terminator)?;
        let stats = cfg.stats.then(|| Stats {
            lines_in,
            lines_out,
            duplicates_removed: lines_in.saturating_sub(lines_out),
            elapsed_secs: start.elapsed().as_secs_f64(),
        });
        return Ok(Outcome {
            exit_code: 0,
            stats,
        });
    }

    // Built once; used for the general path and/or output key highlighting.
    let sorter = cfg.build_sorter().map_err(invalid)?;

    let (ordered, duplicates_removed) = if cfg.is_simple_global() {
        let opts = cfg.key_opts();
        if opts.numeric {
            numeric_order(lines, cfg)
        } else {
            byte_order(lines, cfg, &opts)
        }
    } else {
        general_order(lines, cfg, &sorter)
    };

    let highlight = crate::diag::color_stdout(cfg).then_some(&sorter);
    write_output_with_header(&ordered, header, cfg, terminator, highlight)?;

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

#[inline]
fn stable_for(cfg: &Config) -> bool {
    cfg.stable || cfg.unique
}

/// Plain/fold byte path (no `-n`, no `-k`).
fn byte_order<'a>(
    mut lines: Vec<&'a [u8]>,
    cfg: &Config,
    opts: &KeyOpts,
) -> (Vec<&'a [u8]>, usize) {
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

/// Numeric path with decorate-sort-undecorate (no `-k`).
fn numeric_order<'a>(lines: Vec<&'a [u8]>, cfg: &Config) -> (Vec<&'a [u8]>, usize) {
    let stable = stable_for(cfg);
    let mut dec: Vec<(NumericKey<'a>, &'a [u8])> = lines
        .into_par_iter()
        .map(|l| (NumericKey::parse(l), l))
        .collect();

    let cmp = |a: &(NumericKey, &[u8]), b: &(NumericKey, &[u8])| {
        let mut o = a.0.cmp(&b.0);
        if o == Ordering::Equal && !stable {
            o = a.1.cmp(b.1);
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
    (dec.into_iter().map(|(_, l)| l).collect(), dups)
}

/// General multi-key path driven by a `Sorter` (handles `-k`, `-g/-h/-V/-M`).
fn general_order<'a>(
    mut lines: Vec<&'a [u8]>,
    cfg: &Config,
    sorter: &Sorter,
) -> (Vec<&'a [u8]>, usize) {
    let stable = sorter.suppress_last_resort;
    let cmp = |a: &&[u8], b: &&[u8]| sorter.compare(a, b);

    let fused_top = cfg.top.filter(|_| !cfg.unique);
    match fused_top {
        Some(n) => {
            let n = n.min(lines.len());
            if n == 0 {
                return (Vec::new(), 0);
            }
            if n < lines.len() {
                lines.select_nth_unstable_by(n - 1, |a, b| sorter.compare(a, b));
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
        lines.dedup_by(|a, b| sorter.key_equal(a, b));
        dups = before - lines.len();
        if let Some(n) = cfg.top {
            lines.truncate(n);
        }
    }
    (lines, dups)
}

/// k-way merge of already-sorted inputs (`-m`). Linear min-scan over k heads;
/// ties resolve to the lower-indexed file, preserving merge stability.
fn merge_sorted<'a>(
    per_file: Vec<Vec<&'a [u8]>>,
    cfg: &Config,
    sorter: &Sorter,
) -> (Vec<&'a [u8]>, usize) {
    let k = per_file.len();
    let mut heads = vec![0usize; k];
    let total: usize = per_file.iter().map(|v| v.len()).sum();
    let mut out: Vec<&[u8]> = Vec::with_capacity(total);
    let mut dups = 0;
    loop {
        let mut best: Option<usize> = None;
        for f in 0..k {
            if heads[f] < per_file[f].len() {
                match best {
                    None => best = Some(f),
                    Some(bf) => {
                        if sorter.compare(per_file[f][heads[f]], per_file[bf][heads[bf]])
                            == Ordering::Less
                        {
                            best = Some(f);
                        }
                    }
                }
            }
        }
        let Some(bf) = best else { break };
        let line = per_file[bf][heads[bf]];
        heads[bf] += 1;
        if cfg.unique {
            if let Some(&last) = out.last() {
                if sorter.key_equal(last, line) {
                    dups += 1;
                    continue;
                }
            }
        }
        out.push(line);
    }
    (out, dups)
}

/// Sort stably by key, then run-length-count adjacent equal-key lines, keeping
/// the first line of each group as the representative (matching `sort | uniq -c`
/// for the whole-line case). `--top N` keeps the first N groups in key order.
fn grouped_counts<'a>(
    mut lines: Vec<&'a [u8]>,
    sorter: &Sorter,
    cfg: &Config,
) -> Vec<(u64, &'a [u8])> {
    lines.par_sort_by(|a, b| sorter.compare(a, b));
    let mut out: Vec<(u64, &[u8])> = Vec::new();
    for line in lines {
        match out.last_mut() {
            Some((count, rep)) if sorter.key_equal(rep, line) => *count += 1,
            _ => out.push((1, line)),
        }
    }
    if let Some(n) = cfg.top {
        out.truncate(n);
    }
    out
}

fn write_counts(
    groups: &[(u64, &[u8])],
    header: Option<&[u8]>,
    cfg: &Config,
    terminator: u8,
) -> io::Result<()> {
    let write = |mut w: BufWriter<Box<dyn Write>>| -> io::Result<()> {
        if let Some(h) = header {
            w.write_all(h)?;
            w.write_all(std::slice::from_ref(&terminator))?;
        }
        for (count, line) in groups {
            // GNU `uniq -c` format: count right-justified in 7 columns, a space,
            // then the line.
            write!(w, "{count:>7} ")?;
            w.write_all(line)?;
            w.write_all(std::slice::from_ref(&terminator))?;
        }
        w.flush()
    };
    let sink: Box<dyn Write> = match &cfg.output {
        Some(p) => Box::new(File::create(p)?),
        None => Box::new(io::stdout().lock()),
    };
    write(BufWriter::new(sink))
}

fn check_sorted(lines: &[&[u8]], cfg: &Config, sorter: &Sorter) -> i32 {
    let header_offset = usize::from(cfg.header);
    for (i, win) in lines.windows(2).enumerate() {
        let (prev, cur) = (win[0], win[1]);
        let ord = sorter.check_compare(prev, cur);
        if ord == Ordering::Greater || (cfg.unique && ord == Ordering::Equal) {
            let lineno = i + 2 + header_offset;
            let range = sorter.breaking_key_range(prev, cur);
            crate::diag::report_disorder(cfg, prev, cur, lineno, range);
            return 1;
        }
    }
    0
}

fn write_output_with_header(
    lines: &[&[u8]],
    header: Option<&[u8]>,
    cfg: &Config,
    terminator: u8,
    highlight: Option<&Sorter>,
) -> io::Result<()> {
    match &cfg.output {
        Some(path) => {
            let f = File::create(path)?;
            write_lines(BufWriter::new(f), lines, header, terminator, highlight)
        }
        None => {
            let stdout = io::stdout();
            write_lines(
                BufWriter::new(stdout.lock()),
                lines,
                header,
                terminator,
                highlight,
            )
        }
    }
}

fn write_output(lines: &[&[u8]], cfg: &Config, terminator: u8) -> io::Result<()> {
    write_output_with_header(lines, None, cfg, terminator, None)
}

fn write_lines<W: Write>(
    mut w: W,
    lines: &[&[u8]],
    header: Option<&[u8]>,
    terminator: u8,
    highlight: Option<&Sorter>,
) -> io::Result<()> {
    if let Some(h) = header {
        w.write_all(h)?;
        w.write_all(std::slice::from_ref(&terminator))?;
    }
    match highlight {
        Some(sorter) => {
            for line in lines {
                let range = sorter.first_key_range(line);
                crate::diag::write_highlighted(&mut w, line, range, terminator)?;
            }
        }
        None => {
            for line in lines {
                w.write_all(line)?;
                w.write_all(std::slice::from_ref(&terminator))?;
            }
        }
    }
    w.flush()
}
