//! The sort engine: orchestrates reading, the chosen execution path, and output.

use crate::compare::{compare_key, compare_kind, full_compare, KeyOpts, NumericKey};
use crate::config::Config;
use crate::input::{read_all_with, read_each_with, split_lines};
use crate::key::{extract, Kind, Sorter};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::io::{self, BufWriter, Write};
use std::time::{Duration, Instant};

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
    /// Number of spilled sorted runs when the external (>RAM) path ran;
    /// `None` for the in-memory paths. `> 1` means the input actually spilled.
    pub chunks: Option<usize>,
    /// Wall-clock time elapsed.
    pub elapsed_secs: f64,
}

fn invalid(e: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e)
}

/// Total byte size of all file inputs; `None` when stdin is involved (the
/// length is then unknown, so the bar falls back to a spinner).
fn input_total_bytes(files: &[std::path::PathBuf]) -> Option<u64> {
    if files.is_empty() {
        return None;
    }
    let mut total = 0u64;
    for p in files {
        if p.as_os_str() == "-" {
            return None;
        }
        total += std::fs::metadata(p).ok()?.len();
    }
    Some(total)
}

/// Build the read-phase progress bar for a known (or unknown) total input size.
/// Split out from [`make_progress`] so it is exercisable without a TTY.
fn build_progress_bar(total: Option<u64>) -> ProgressBar {
    let pb = match total {
        Some(total) => {
            let pb = ProgressBar::new(total);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} {bytes}/{total_bytes} ({percent}%) [{bar:30}] ETA {eta} {msg}",
                )
                .unwrap()
                .progress_chars("=> "),
            );
            pb
        }
        None => {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template("{spinner:.green} {bytes} read {msg}").unwrap(),
            );
            pb
        }
    };
    pb.set_message("reading");
    pb
}

/// Build a read-phase progress bar when `--progress` is set and stderr is a
/// terminal. Returns `None` otherwise (zero overhead on the common path).
pub(crate) fn make_progress(cfg: &Config) -> Option<ProgressBar> {
    if !crate::diag::progress_enabled(cfg) {
        return None;
    }
    Some(build_progress_bar(input_total_bytes(&cfg.files)))
}

/// Switch a read bar to an indeterminate spinner for the sort/merge phase.
fn progress_phase(pb: &Option<ProgressBar>, msg: &'static str) {
    if let Some(pb) = pb {
        pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg}").unwrap());
        pb.set_message(msg);
        pb.enable_steady_tick(Duration::from_millis(100));
    }
}

/// Clear the progress bar from stderr before any stats/diagnostics print.
fn finish_progress(pb: &Option<ProgressBar>) {
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
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
            let pb = make_progress(cfg);
            let (lines_in, lines_out, chunks) =
                crate::external::run_external(cfg, &sorter, budget, terminator, pb.as_ref())?;
            finish_progress(&pb);
            let stats = cfg.stats.then(|| Stats {
                lines_in,
                lines_out,
                duplicates_removed: lines_in.saturating_sub(lines_out),
                chunks: Some(chunks),
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
        let pb = make_progress(cfg);
        let sorter = cfg.build_sorter().map_err(invalid)?;
        let buffers = read_each_with(&cfg.files, terminator, pb.as_ref())?;
        let per_file: Vec<Vec<&[u8]>> =
            buffers.iter().map(|b| split_lines(b, terminator)).collect();
        let lines_in: usize = per_file.iter().map(|v| v.len()).sum();
        progress_phase(&pb, "merging");
        let (ordered, dups) = merge_sorted(per_file, cfg, &sorter);
        finish_progress(&pb);
        write_output(&ordered, cfg, terminator)?;
        let stats = cfg.stats.then(|| Stats {
            lines_in,
            lines_out: ordered.len(),
            duplicates_removed: dups,
            chunks: None,
            elapsed_secs: start.elapsed().as_secs_f64(),
        });
        return Ok(Outcome {
            exit_code: 0,
            stats,
        });
    }

    let pb = make_progress(cfg);
    let data = read_all_with(&cfg.files, terminator, pb.as_ref())?;
    let mut lines = split_lines(&data, terminator);

    // --header: peel off the first line, pin it on output.
    let header = if cfg.header && !lines.is_empty() {
        Some(lines.remove(0))
    } else {
        None
    };
    let lines_in = lines.len();

    if cfg.check {
        finish_progress(&pb);
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
        progress_phase(&pb, "sorting");
        let sorter = cfg.build_sorter().map_err(invalid)?;
        let groups = grouped_counts(lines, &sorter, cfg);
        finish_progress(&pb);
        let lines_out = groups.len();
        write_counts(&groups, header, cfg, terminator)?;
        let stats = cfg.stats.then(|| Stats {
            lines_in,
            lines_out,
            duplicates_removed: lines_in.saturating_sub(lines_out),
            chunks: None,
            elapsed_secs: start.elapsed().as_secs_f64(),
        });
        return Ok(Outcome {
            exit_code: 0,
            stats,
        });
    }

    // Built once; used for the general path and/or output key highlighting.
    let sorter = cfg.build_sorter().map_err(invalid)?;

    progress_phase(&pb, "sorting");
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
    finish_progress(&pb);

    let highlight = crate::diag::color_stdout(cfg).then_some(&sorter);
    write_output_with_header(&ordered, header, cfg, terminator, highlight)?;

    let stats = cfg.stats.then(|| Stats {
        lines_in,
        lines_out: ordered.len(),
        duplicates_removed,
        chunks: None,
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

    // The select_nth fast path reorders equal elements, so it can only be used
    // when neither stability nor uniqueness is requested; otherwise fall back to
    // a full (stable, when required) sort and truncate afterwards.
    let fused_top = cfg.top.filter(|_| !cfg.unique && !stable);
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
    }
    if let Some(n) = cfg.top {
        lines.truncate(n);
    }
    (lines, dups)
}

/// Numeric path with decorate-sort-undecorate (no `-k`).
fn numeric_order<'a>(lines: Vec<&'a [u8]>, cfg: &Config) -> (Vec<&'a [u8]>, usize) {
    // Integer fast path: when every key is an exact i64, a stable LSD radix
    // sort replaces the comparison sort with byte-identical output. Any
    // non-integer key makes this return None and we fall back below.
    if let Some(res) = crate::radix::try_numeric_radix(&lines, cfg) {
        return res;
    }

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

    // See `byte_order`: the unstable select_nth shortcut is unsound when
    // stability or uniqueness is requested.
    let fused_top = cfg.top.filter(|_| !cfg.unique && !stable);
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
    }
    if let Some(n) = cfg.top {
        dec.truncate(n);
    }
    (dec.into_iter().map(|(_, l)| l).collect(), dups)
}

/// A precomputed sort key: numeric and date keys are parsed once up front,
/// everything else is kept as a zero-copy slice into the line.
enum Dec<'a> {
    Num(NumericKey<'a>),
    /// Parsed timestamp in nanoseconds; `i128::MIN` marks an unparseable value
    /// (which sorts first, matching `datetime_cmp`).
    Date(i128),
    Slice(&'a [u8]),
}

/// Decorate a key's extracted bytes into a [`Dec`], parsing numeric/date keys
/// once up front so comparisons are O(1). Shared by the single- and multi-key
/// paths so the two cannot decorate inconsistently.
#[inline]
fn decorate(kb: &[u8], kind: Kind) -> Dec<'_> {
    match kind {
        Kind::Numeric => Dec::Num(NumericKey::parse(kb)),
        Kind::DateTime => Dec::Date(crate::compare::parse_datetime(kb).unwrap_or(i128::MIN)),
        _ => Dec::Slice(kb),
    }
}

/// Single-key DSU fast path. The precomputed key is stored *inline* with the
/// line in one contiguous record, so a comparison touches only the record it
/// already loaded — avoiding the extra indirection (and cache misses) of the
/// multi-key flat array. This special case is load-bearing for the common
/// "sort by one column" workload (~1.8x faster than routing it through
/// `general_order`); do not fold it away. The tie-break uses `Sorter::finish`,
/// shared with `Sorter::compare`, so the ordering contract cannot drift.
fn single_key_order<'a>(
    lines: Vec<&'a [u8]>,
    cfg: &Config,
    sorter: &Sorter,
) -> (Vec<&'a [u8]>, usize) {
    let key = &sorter.keys[0];
    let tab = sorter.tab;
    let (kind, fold, key_reverse) = (key.kind, key.fold, key.reverse);
    let stable = sorter.suppress_last_resort;

    let key_cmp = |a: &Dec<'a>, b: &Dec<'a>| match (a, b) {
        (Dec::Num(x), Dec::Num(y)) => x.cmp(y),
        (Dec::Date(x), Dec::Date(y)) => x.cmp(y),
        (Dec::Slice(x), Dec::Slice(y)) => compare_kind(x, y, kind, fold),
        _ => Ordering::Equal, // unreachable: every line uses the same variant
    };
    let full_cmp = |a: &(Dec<'a>, &'a [u8]), b: &(Dec<'a>, &'a [u8])| {
        let mut o = key_cmp(&a.0, &b.0);
        if key_reverse {
            o = o.reverse();
        }
        sorter.finish(o, a.1, b.1)
    };

    let mut dec: Vec<(Dec<'a>, &'a [u8])> = lines
        .into_par_iter()
        .map(|l| {
            let kb = extract(l, key, tab);
            (decorate(kb, kind), l)
        })
        .collect();

    // See `byte_order`: the unstable select_nth shortcut is unsound when
    // stability or uniqueness is requested. Here `stable` is
    // `suppress_last_resort`, which is set by `-s` or `-u`, so the single
    // `!stable` guard already covers both.
    let fused_top = cfg.top.filter(|_| !stable);
    match fused_top {
        Some(n) => {
            let n = n.min(dec.len());
            if n == 0 {
                return (Vec::new(), 0);
            }
            if n < dec.len() {
                dec.select_nth_unstable_by(n - 1, full_cmp);
                dec.truncate(n);
            }
            dec.par_sort_unstable_by(full_cmp);
        }
        None if stable => dec.par_sort_by(full_cmp),
        None => dec.par_sort_unstable_by(full_cmp),
    }

    let mut dups = 0;
    if cfg.unique {
        let before = dec.len();
        dec.dedup_by(|a, b| key_cmp(&a.0, &b.0) == Ordering::Equal);
        dups = before - dec.len();
    }
    if let Some(n) = cfg.top {
        dec.truncate(n);
    }
    (dec.into_iter().map(|(_, l)| l).collect(), dups)
}

/// Multi-key DSU path (`-k` with 2+ keys, `-g/-h/-V/-M`). Every key of every
/// line is extracted — and, for `-n`, parsed — exactly once into a flat array
/// (`dec[i*k + j]` is line `i`'s key `j`), so the sort does no per-comparison
/// extraction and no per-line allocation. Records carry just an index into that
/// array plus the line. The tie-break goes through `Sorter::finish`, the same
/// last-resort/reverse logic as `Sorter::compare`, so the two cannot drift.
fn general_order<'a>(
    lines: Vec<&'a [u8]>,
    cfg: &Config,
    sorter: &Sorter,
) -> (Vec<&'a [u8]>, usize) {
    // One key: the inline-record fast path has better cache locality.
    if sorter.keys.len() == 1 {
        return single_key_order(lines, cfg, sorter);
    }
    let k = sorter.keys.len();
    let tab = sorter.tab;
    let stable = sorter.suppress_last_resort;
    let specs: Vec<(Kind, bool, bool)> = sorter
        .keys
        .iter()
        .map(|key| (key.kind, key.fold, key.reverse))
        .collect();

    let n = lines.len();
    let dec: Vec<Dec> = (0..n * k)
        .into_par_iter()
        .map(|idx| {
            let key = &sorter.keys[idx % k];
            let kb = extract(lines[idx / k], key, tab);
            decorate(kb, key.kind)
        })
        .collect();

    let key_cmp = |ba: usize, bb: usize| -> Ordering {
        for (j, &(kind, fold, rev)) in specs.iter().enumerate() {
            let mut o = match (&dec[ba + j], &dec[bb + j]) {
                (Dec::Num(x), Dec::Num(y)) => x.cmp(y),
                (Dec::Date(x), Dec::Date(y)) => x.cmp(y),
                (Dec::Slice(x), Dec::Slice(y)) => compare_kind(x, y, kind, fold),
                _ => Ordering::Equal, // unreachable: key j is the same variant for all lines
            };
            if rev {
                o = o.reverse();
            }
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    };
    let full_cmp =
        |a: &(usize, &[u8]), b: &(usize, &[u8])| sorter.finish(key_cmp(a.0, b.0), a.1, b.1);

    let mut recs: Vec<(usize, &[u8])> = (0..n).map(|i| (i * k, lines[i])).collect();

    // See `byte_order`: the unstable select_nth shortcut is unsound when
    // stability or uniqueness is requested. Here `stable` is
    // `suppress_last_resort`, which is set by `-s` or `-u`, so the single
    // `!stable` guard already covers both.
    let fused_top = cfg.top.filter(|_| !stable);
    match fused_top {
        Some(top) => {
            let top = top.min(recs.len());
            if top == 0 {
                return (Vec::new(), 0);
            }
            if top < recs.len() {
                recs.select_nth_unstable_by(top - 1, full_cmp);
                recs.truncate(top);
            }
            recs.par_sort_unstable_by(full_cmp);
        }
        None if stable => recs.par_sort_by(full_cmp),
        None => recs.par_sort_unstable_by(full_cmp),
    }

    let mut dups = 0;
    if cfg.unique {
        let before = recs.len();
        recs.dedup_by(|a, b| key_cmp(a.0, b.0) == Ordering::Equal);
        dups = before - recs.len();
    }
    if let Some(top) = cfg.top {
        recs.truncate(top);
    }
    (recs.into_iter().map(|(_, l)| l).collect(), dups)
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
        Some(p) => crate::compress::create_output(p)?,
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
            // Compress by output extension (stdout is never compressed); errors
            // carry the path.
            let w = crate::compress::create_output(path)?;
            write_lines(BufWriter::new(w), lines, header, terminator, highlight)
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

#[cfg(test)]
mod tests {
    use super::{build_progress_bar, finish_progress, input_total_bytes, progress_phase};
    use indicatif::ProgressDrawTarget;
    use std::path::PathBuf;

    /// A hidden bar so tests never draw to stderr.
    fn hidden(total: Option<u64>) -> indicatif::ProgressBar {
        let pb = build_progress_bar(total);
        pb.set_draw_target(ProgressDrawTarget::hidden());
        pb
    }

    #[test]
    fn total_bytes_stdin_and_empty_are_none() {
        assert_eq!(input_total_bytes(&[]), None); // stdin
        assert_eq!(input_total_bytes(&[PathBuf::from("-")]), None); // explicit stdin
        assert_eq!(input_total_bytes(&[PathBuf::from("/no/such/file")]), None);
    }

    #[test]
    fn total_bytes_sums_real_files() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("xort_tb_{}", std::process::id()));
        std::fs::write(&p, b"hello\n").unwrap();
        assert_eq!(input_total_bytes(std::slice::from_ref(&p)), Some(6));
        // A "-" anywhere makes the total unknown.
        assert_eq!(input_total_bytes(&[p.clone(), PathBuf::from("-")]), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn progress_bar_both_styles_drive_and_clear() {
        // Byte-total style.
        let pb = hidden(Some(100));
        pb.set_position(50);
        progress_phase(&Some(pb.clone()), "sorting");
        finish_progress(&Some(pb));
        // Spinner style (unknown length).
        let sp = hidden(None);
        sp.inc(10);
        progress_phase(&Some(sp.clone()), "merging");
        finish_progress(&Some(sp));
        // None is a no-op on both helpers.
        progress_phase(&None, "sorting");
        finish_progress(&None);
    }
}
