//! Black-box integration tests driving the compiled `xort` binary.

use std::io::Write;
use std::process::{Command, Stdio};

fn xort(args: &[&str], stdin: &[u8]) -> (Vec<u8>, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xort"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn xort");
    // Ignore broken-pipe errors: commands that fail during argument parsing
    // (e.g. an invalid -t) exit before reading stdin.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    let out = child.wait_with_output().expect("wait");
    (out.stdout, out.status.code().unwrap_or(-1))
}

fn run(args: &[&str], stdin: &str) -> String {
    let (out, code) = xort(args, stdin.as_bytes());
    assert_eq!(code, 0, "expected success for args {args:?}");
    String::from_utf8(out).unwrap()
}

#[test]
fn plain_sort() {
    assert_eq!(
        run(&[], "banana\napple\ncherry\n"),
        "apple\nbanana\ncherry\n"
    );
}

#[test]
fn reverse() {
    assert_eq!(run(&["-r"], "a\nc\nb\n"), "c\nb\na\n");
}

#[test]
fn numeric() {
    assert_eq!(run(&["-n"], "10\n2\n1\n"), "1\n2\n10\n");
}

#[test]
fn numeric_negatives_and_floats() {
    assert_eq!(run(&["-n"], "-1\n2.5\n-3\n2.05\n"), "-3\n-1\n2.05\n2.5\n");
}

#[test]
fn unique() {
    assert_eq!(run(&["-u"], "b\na\nb\na\n"), "a\nb\n");
}

#[test]
fn unique_fold_keeps_first_input_occurrence() {
    // GNU disables the last-resort comparison under -u, keeping the first line
    // in input order among fold-equal lines.
    assert_eq!(
        run(&["-f", "-u"], "banana\nBanana\napple\n"),
        "apple\nbanana\n"
    );
    assert_eq!(
        run(&["-f", "-u"], "Banana\nbanana\napple\n"),
        "apple\nBanana\n"
    );
}

#[test]
fn fold_case_last_resort_when_not_unique() {
    // Not unique: keys fold-equal, last-resort byte order puts 'B' before 'b'.
    assert_eq!(run(&["-f"], "banana\nBanana\n"), "Banana\nbanana\n");
}

#[test]
fn stable_preserves_input_order() {
    // Equal numeric keys; -s keeps input order of the differing representations.
    assert_eq!(run(&["-n", "-s"], "1.0\n1.00\n1\n"), "1.0\n1.00\n1\n");
}

#[test]
fn ignore_leading_blanks() {
    assert_eq!(run(&["-b"], "  b\na\n"), "a\n  b\n");
}

#[test]
fn top_n() {
    assert_eq!(run(&["-n", "--top", "3"], "5\n3\n9\n1\n7\n"), "1\n3\n5\n");
}

#[test]
fn top_n_unique_means_n_distinct() {
    assert_eq!(run(&["--top", "2", "-u"], "c\na\na\nb\nb\n"), "a\nb\n");
}

#[test]
fn zero_terminated() {
    let (out, code) = xort(&["-z"], b"b\0a\0c\0");
    assert_eq!(code, 0);
    assert_eq!(out, b"a\0b\0c\0");
}

#[test]
fn check_sorted_exit_codes() {
    let (_, ok) = xort(&["-c"], b"a\nb\nc\n");
    assert_eq!(ok, 0);
    let (_, bad) = xort(&["-c"], b"a\nc\nb\n");
    assert_eq!(bad, 1);
}

#[test]
fn multiple_files_merge() {
    // Two files, the first lacking a trailing newline, must not merge lines.
    let dir = std::env::temp_dir();
    let f1 = dir.join("xort_it_f1.txt");
    let f2 = dir.join("xort_it_f2.txt");
    std::fs::write(&f1, b"banana\napple").unwrap(); // no trailing newline
    std::fs::write(&f2, b"cherry\n").unwrap();
    let out = run(&[f1.to_str().unwrap(), f2.to_str().unwrap()], "");
    assert_eq!(out, "apple\nbanana\ncherry\n");
    let _ = std::fs::remove_file(f1);
    let _ = std::fs::remove_file(f2);
}

// --- M2: field keys, type comparators, merge, external sort, header ---------

#[test]
fn key_field_numeric() {
    // sort by 2nd whitespace field numerically
    assert_eq!(run(&["-k2,2n"], "c 1\na 3\nb 2\n"), "c 1\nb 2\na 3\n");
}

#[test]
fn key_field_tab() {
    assert_eq!(
        run(&["-t:", "-k2,2n"], "c:1\na:3\nb:2\n"),
        "c:1\nb:2\na:3\n"
    );
}

#[test]
fn multi_key() {
    // primary field 1 (text), then field 2 numeric
    assert_eq!(
        run(&["-k1,1", "-k2,2n"], "a 10\nb 1\na 2\n"),
        "a 2\na 10\nb 1\n"
    );
}

#[test]
fn general_numeric() {
    assert_eq!(run(&["-g"], "1e3\n50\n2.5e1\n"), "2.5e1\n50\n1e3\n");
}

#[test]
fn human_numeric() {
    assert_eq!(run(&["-h"], "2K\n1G\n500\n3M\n"), "500\n2K\n3M\n1G\n");
}

#[test]
fn version_sort() {
    assert_eq!(run(&["-V"], "v10\nv2\nv1\n"), "v1\nv2\nv10\n");
}

#[test]
fn month_sort() {
    assert_eq!(run(&["-M"], "Mar\nJan\nFeb\n"), "Jan\nFeb\nMar\n");
}

#[test]
fn header_pins_first_line() {
    assert_eq!(
        run(&["--header"], "name\nzed\nabe\nmary\n"),
        "name\nabe\nmary\nzed\n"
    );
}

#[test]
fn merge_presorted() {
    use std::io::Write as _;
    let dir = std::env::temp_dir();
    let f1 = dir.join("xort_m1.txt");
    let f2 = dir.join("xort_m2.txt");
    std::fs::File::create(&f1)
        .unwrap()
        .write_all(b"a\nc\ne\n")
        .unwrap();
    std::fs::File::create(&f2)
        .unwrap()
        .write_all(b"b\nd\nf\n")
        .unwrap();
    let out = run(&["-m", f1.to_str().unwrap(), f2.to_str().unwrap()], "");
    assert_eq!(out, "a\nb\nc\nd\ne\nf\n");
    let _ = std::fs::remove_file(f1);
    let _ = std::fs::remove_file(f2);
}

#[test]
fn external_sort_matches_inmemory() {
    use std::io::Write as _;
    let dir = std::env::temp_dir();
    let f = dir.join("xort_ext.txt");
    let mut data = String::new();
    // deterministic pseudo-random ints
    let mut x: u64 = 88172645463325252;
    for _ in 0..20000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        data.push_str(&(x % 100000).to_string());
        data.push('\n');
    }
    std::fs::File::create(&f)
        .unwrap()
        .write_all(data.as_bytes())
        .unwrap();
    let inmem = run(&["-n", f.to_str().unwrap()], "");
    let external = run(&["-n", "-S", "8K", f.to_str().unwrap()], "");
    assert_eq!(inmem, external, "external sort must match in-memory output");
    let _ = std::fs::remove_file(f);
}

#[test]
fn human_short_help_not_consumed() {
    // -h must be human-numeric, not help (help is --help only).
    let (out, code) = xort(&["-h"], b"2K\n500\n");
    assert_eq!(code, 0);
    assert_eq!(out, b"500\n2K\n");
}

// --- M3: fused dedup + count ------------------------------------------------

#[test]
fn count_like_uniq_c() {
    assert_eq!(
        run(
            &["--count"],
            "banana\napple\nbanana\ncherry\napple\nbanana\n"
        ),
        "      2 apple\n      3 banana\n      1 cherry\n"
    );
}

#[test]
fn count_with_top_keeps_first_n_groups() {
    // groups a=2, b=3, c=1; sorted a,b,c; top 2 keeps a and b.
    assert_eq!(
        run(&["--count", "--top", "2"], "c\na\na\nb\nb\nb\n"),
        "      2 a\n      3 b\n"
    );
}

// --- M4: structured formats (CSV / TSV / JSON / JSONL) ----------------------

#[test]
fn csv_sort_by_column_name_numeric() {
    let out = run(
        &["--csv", "--header", "-k", "age", "-n"],
        "name,age\nbob,40\nalice,25\ncarol,30\n",
    );
    assert_eq!(out, "name,age\nalice,25\ncarol,30\nbob,40\n");
}

#[test]
fn csv_quoted_field_not_mangled() {
    // The quoted comma must not split the row; sort by age (col 2) numeric.
    let out = run(
        &["--csv", "--header", "-k2n"],
        "name,age\n\"Smith, John\",30\nAlice,25\n",
    );
    assert_eq!(out, "name,age\nAlice,25\n\"Smith, John\",30\n");
}

#[test]
fn tsv_sort_by_column() {
    assert_eq!(
        run(&["--tsv", "-k2n"], "a\t3\nb\t1\nc\t2\n"),
        "b\t1\nc\t2\na\t3\n"
    );
}

#[test]
fn jsonl_sort_by_field_preserves_key_order() {
    let out = run(
        &["--jsonl", "-k", ".age"],
        "{\"name\":\"bob\",\"age\":40}\n{\"name\":\"alice\",\"age\":25}\n",
    );
    assert_eq!(
        out,
        "{\"name\":\"alice\",\"age\":25}\n{\"name\":\"bob\",\"age\":40}\n"
    );
}

#[test]
fn json_array_sorted_by_field() {
    let out = run(
        &["--json", "-k", "age"],
        "[{\"age\":5},{\"age\":50},{\"age\":9}]",
    );
    // numeric ordering (not lexical): 5, 9, 50
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ages: Vec<i64> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["age"].as_i64().unwrap())
        .collect();
    assert_eq!(ages, vec![5, 9, 50]);
}

// --- M5: UX polish ----------------------------------------------------------

#[test]
fn color_never_has_no_escapes() {
    let out = run(&["--color=never"], "b\na\n");
    assert_eq!(out, "a\nb\n");
    assert!(!out.contains('\x1b'));
}

#[test]
fn color_always_highlights() {
    let out = run(&["--color=always"], "b\na\n");
    assert!(out.contains('\x1b'), "forced color must emit ANSI");
}

#[test]
fn rich_check_reports_and_exits_1() {
    let (out, code) = xort(&["-c", "-k2,2n"], b"a 1\nb 5\nc 3\n");
    assert_eq!(code, 1);
    // diagnostics go to stderr; stdout stays empty
    assert!(out.is_empty());
}

#[test]
fn completions_generate() {
    let (out, code) = xort(&["--completions", "bash"], b"");
    assert_eq!(code, 0);
    assert!(!out.is_empty());
}

#[test]
fn man_page_generates() {
    let (out, code) = xort(&["--man"], b"");
    assert_eq!(code, 0);
    assert!(String::from_utf8_lossy(&out).contains(".TH xort"));
}

// --- coverage: error paths, output file, stats, more format/engine branches -

#[test]
fn csv_unique_and_whole_record() {
    // no -k => whole-record key; -u dedups identical rows
    assert_eq!(run(&["--csv", "-u"], "a,1\nb,2\na,1\n"), "a,1\nb,2\n");
}

#[test]
fn csv_unknown_column_name_errors() {
    let (_, code) = xort(&["--csv", "--header", "-k", "nope"], b"name,age\nx,1\n");
    assert_eq!(code, 2, "unknown column name should exit 2");
}

#[test]
fn csv_name_without_header_errors() {
    let (_, code) = xort(&["--csv", "-k", "age"], b"name,age\nx,1\n");
    assert_eq!(code, 2);
}

#[test]
fn json_reverse_and_unique() {
    let out = run(
        &["--jsonl", "-k", ".v", "-r"],
        "{\"v\":1}\n{\"v\":3}\n{\"v\":2}\n",
    );
    assert_eq!(out, "{\"v\":3}\n{\"v\":2}\n{\"v\":1}\n");
    let uniq = run(
        &["--jsonl", "-k", ".v", "-u"],
        "{\"v\":1}\n{\"v\":1}\n{\"v\":2}\n",
    );
    assert_eq!(uniq, "{\"v\":1}\n{\"v\":2}\n");
}

#[test]
fn invalid_json_errors() {
    let (_, code) = xort(&["--json", "-k", "x"], b"{not valid json");
    assert_eq!(code, 2);
}

#[test]
fn multichar_tab_errors() {
    let (_, code) = xort(&["-t", "ab"], b"x\n");
    assert_eq!(code, 2);
}

#[test]
fn zero_terminated_with_csv_errors() {
    let (_, code) = xort(&["--csv", "-z"], b"a,b\n");
    assert_eq!(code, 2);
}

#[test]
fn output_to_file_and_stats() {
    let dir = std::env::temp_dir();
    let out = dir.join("xort_out.txt");
    let (_, code) = xort(
        &["-n", "--stats", "-o", out.to_str().unwrap()],
        b"3\n1\n2\n",
    );
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "1\n2\n3\n");
    let _ = std::fs::remove_file(out);
}

#[test]
fn merge_unique() {
    use std::io::Write as _;
    let dir = std::env::temp_dir();
    let f1 = dir.join("xort_mu1.txt");
    let f2 = dir.join("xort_mu2.txt");
    std::fs::File::create(&f1)
        .unwrap()
        .write_all(b"a\nb\n")
        .unwrap();
    std::fs::File::create(&f2)
        .unwrap()
        .write_all(b"b\nc\n")
        .unwrap();
    let out = run(
        &["-m", "-u", f1.to_str().unwrap(), f2.to_str().unwrap()],
        "",
    );
    assert_eq!(out, "a\nb\nc\n");
    let _ = std::fs::remove_file(f1);
    let _ = std::fs::remove_file(f2);
}

#[test]
fn count_with_header() {
    assert_eq!(
        run(&["--count", "--header"], "label\nb\na\nb\n"),
        "label\n      1 a\n      2 b\n"
    );
}

#[test]
fn external_unique_with_tempdir() {
    use std::io::Write as _;
    let dir = std::env::temp_dir();
    let f = dir.join("xort_extu.txt");
    let mut s = String::new();
    for i in 0..5000 {
        s.push_str(&((i % 500).to_string()));
        s.push('\n');
    }
    std::fs::File::create(&f)
        .unwrap()
        .write_all(s.as_bytes())
        .unwrap();
    let out = run(
        &[
            "-n",
            "-u",
            "-S",
            "4K",
            "-T",
            dir.to_str().unwrap(),
            f.to_str().unwrap(),
        ],
        "",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 500);
    assert_eq!(lines[0], "0");
    assert_eq!(lines[499], "499");
    let _ = std::fs::remove_file(f);
}

#[test]
fn reverse_general_human_month_via_keys() {
    // exercise the general_order path with typed keys + reverse
    assert_eq!(run(&["-hr"], "1K\n2K\n500\n"), "2K\n1K\n500\n");
    assert_eq!(run(&["-Mr"], "Jan\nMar\nFeb\n"), "Mar\nFeb\nJan\n");
}

// --- coverage: stdin variants and IO error paths ---------------------------

#[test]
fn stdin_dash_reads_stdin() {
    assert_eq!(run(&["-"], "b\na\nc\n"), "a\nb\nc\n");
}

#[test]
fn merge_from_stdin_single_source() {
    // -m with no files reads stdin as one already-sorted source.
    assert_eq!(run(&["-m"], "a\nb\nc\n"), "a\nb\nc\n");
}

#[test]
fn missing_file_errors() {
    let (_, code) = xort(&["/no/such/xort/file"], b"");
    assert_eq!(code, 2);
}

#[test]
fn external_from_stdin() {
    // -S with stdin exercises the streaming LineSource over stdin.
    assert_eq!(run(&["-n", "-S", "16"], "30\n10\n20\n"), "10\n20\n30\n");
}

// --- M5/bench: --stats reports spilled chunks on the external path ----------

/// Run capturing stderr too (the helper above discards it).
fn xort_stderr(args: &[&str], stdin: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xort"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xort");
    let _ = child.stdin.take().unwrap().write_all(stdin);
    let out = child.wait_with_output().expect("wait");
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

#[test]
fn stats_reports_spilled_chunks_when_external() {
    // Many lines + a tiny buffer forces multiple spilled runs.
    let mut input = String::new();
    for i in (0..20000).rev() {
        input.push_str(&i.to_string());
        input.push('\n');
    }
    let (out, err, code) = xort_stderr(&["-n", "-S", "16K", "--stats"], input.as_bytes());
    assert_eq!(code, 0);
    assert_eq!(&out[..6], b"0\n1\n2\n"); // sorted ascending
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("spilled chunk(s)"), "stderr was: {err}");
    // Extract the chunk count and assert it actually spilled (> 1).
    let n: usize = err
        .split(", ")
        .find_map(|s| s.strip_suffix(" spilled chunk(s)"))
        .and_then(|s| s.parse().ok())
        .expect("chunk count in stats");
    assert!(n > 1, "expected multiple spilled chunks, got {n}");
}

#[test]
fn stats_no_chunks_for_in_memory() {
    let (_, err, code) = xort_stderr(&["-n", "--stats"], b"3\n1\n2\n");
    assert_eq!(code, 0);
    let err = String::from_utf8(err).unwrap();
    assert!(!err.contains("spilled chunk"), "stderr was: {err}");
}

// --- regressions: external (-S) correctness (code-review findings) ----------

#[test]
fn external_unique_single_chunk_dedups() {
    // A large -S keeps everything in one chunk; the single-chunk fast path
    // must still apply -u (regression: it bypassed merge_runs' dedup).
    assert_eq!(
        run(&["-n", "-u", "-S", "1G"], "3\n1\n2\n1\n3\n2\n"),
        "1\n2\n3\n"
    );
}

#[test]
fn external_unique_stats_counts() {
    let (_, err, code) = xort_stderr(&["-n", "-u", "-S", "1G", "--stats"], b"3\n1\n2\n1\n3\n2\n");
    assert_eq!(code, 0);
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.contains("6 in, 3 out, 3 duplicate"),
        "external -u stats should reflect dedup; got: {err}"
    );
}

#[test]
fn external_multifile_missing_trailing_newline() {
    // A file lacking a trailing newline must not glue onto the next file's
    // first line under -S (regression: Read::chain concatenated them).
    let dir = std::env::temp_dir();
    let f1 = dir.join("xort_extmf1.txt");
    let f2 = dir.join("xort_extmf2.txt");
    std::fs::write(&f1, b"banana\napple").unwrap(); // no trailing newline
    std::fs::write(&f2, b"cherry\n").unwrap();
    let out = run(
        &["-S", "1G", f1.to_str().unwrap(), f2.to_str().unwrap()],
        "",
    );
    assert_eq!(out, "apple\nbanana\ncherry\n");
    let _ = std::fs::remove_file(f1);
    let _ = std::fs::remove_file(f2);
}

#[test]
fn external_line_longer_than_buffer() {
    // A single line larger than the -S budget must still sort correctly
    // (read_block keeps reading until it finds a terminator).
    let big = "z".repeat(5000);
    let input = format!("{big}\nb\na\n");
    let out = run(&["-S", "100", "-"], &input);
    assert_eq!(out, format!("a\nb\n{big}\n"));
}

// --- Review-driven regressions ---------------------------------------------

#[test]
fn external_output_aliasing_input_preserves_data() {
    // `-S ... -o FILE FILE`: the output must not be truncated before the input
    // is read, or the file is destroyed instead of sorted.
    use std::io::Write as _;
    let dir = std::env::temp_dir();
    let f = dir.join("xort_alias.txt");
    std::fs::File::create(&f)
        .unwrap()
        .write_all(b"3\n1\n2\n")
        .unwrap();
    let path = f.to_str().unwrap();
    let (_, code) = xort(&["-n", "-S", "1G", "-o", path, path], b"");
    assert_eq!(code, 0);
    let got = std::fs::read_to_string(&f).unwrap();
    let _ = std::fs::remove_file(&f);
    assert_eq!(got, "1\n2\n3\n");
}

#[test]
fn jsonl_unique_without_key_keeps_distinct_records() {
    // No key => sort by whole value; -u must only drop true duplicates, not
    // collapse every record into one.
    let out = run(&["--jsonl", "-u"], "{\"x\":2}\n{\"x\":1}\n{\"x\":2}\n");
    assert_eq!(out, "{\"x\":1}\n{\"x\":2}\n");
}

#[test]
fn top_with_stable_is_stable() {
    // -s must preserve input order among equal keys even with --top, matching
    // `sort -f -s | head -n 2`.
    assert_eq!(
        run(
            &["-f", "-s", "--top", "2"],
            "banana\nBanana\nBANANA\napple\n"
        ),
        "apple\nbanana\n"
    );
}

#[test]
fn top_with_stable_key_is_stable() {
    // Same, routed through the keyed (single-key) path.
    assert_eq!(
        run(&["-k1,1", "-s", "--top", "2"], "1 c\n1 b\n1 a\n2 z\n"),
        "1 c\n1 b\n"
    );
}

#[test]
fn csv_column_index_inherits_global_type() {
    // `-k2 -n` (no inline options) must inherit the global numeric type.
    let out = run(&["--csv", "--header", "-k2", "-n"], "n,v\na,10\nb,2\n");
    assert_eq!(out, "n,v\nb,2\na,10\n");
}

#[test]
fn csv_column_range_options_not_dropped() {
    // `-k2,2n` must apply the `n` option rather than silently discarding it.
    let out = run(&["--csv", "--header", "-k2,2n"], "n,v\na,10\nb,2\n");
    assert_eq!(out, "n,v\nb,2\na,10\n");
}

#[test]
fn csv_multicolumn_range_errors() {
    let (_, code) = xort(&["--csv", "--header", "-k1,2"], b"a,b\nx,y\n");
    assert_eq!(code, 2, "multi-column CSV range should exit 2");
}

#[test]
fn invalid_key_option_letter_errors() {
    let (_, code) = xort(&["-k1x"], b"b\na\n");
    assert_eq!(code, 2, "unknown -k option letter should exit 2");
}

#[test]
fn invalid_key_dangling_dot_errors() {
    let (_, code) = xort(&["-k1."], b"b\na\n");
    assert_eq!(code, 2, "-k with a '.' and no char position should exit 2");
}

// --- coverage: engine dispatch branches ------------------------------------

#[test]
fn parallel_flag_sets_thread_pool() {
    // --parallel builds the rayon global pool; output must still be correct.
    assert_eq!(run(&["-n", "--parallel", "2"], "3\n1\n2\n"), "1\n2\n3\n");
}

#[test]
fn plain_top_without_type() {
    // No -n/-k: the byte_order fused select_nth fast path (n < len).
    assert_eq!(
        run(&["--top", "2"], "delta\nalpha\ncharlie\nbravo\n"),
        "alpha\nbravo\n"
    );
}

#[test]
fn top_zero_emits_nothing() {
    // --top 0 returns an empty result across every order path.
    assert_eq!(run(&["--top", "0"], "b\na\n"), ""); // byte path
    assert_eq!(run(&["-n", "--top", "0"], "2\n1\n"), ""); // numeric path
    assert_eq!(run(&["-k1,1", "--top", "0"], "b x\na y\n"), ""); // single-key path
    assert_eq!(run(&["-k1,1", "-k2,2", "--top", "0"], "b x\na y\n"), ""); // multi-key path
}

#[test]
fn merge_with_stats() {
    let dir = tempfile::tempdir().unwrap();
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.txt");
    std::fs::write(&f1, b"a\nc\n").unwrap();
    std::fs::write(&f2, b"b\nd\n").unwrap();
    let (out, err, code) = xort_stderr(
        &[
            "-m",
            "--stats",
            &f1.to_string_lossy(),
            &f2.to_string_lossy(),
        ],
        b"",
    );
    assert_eq!(code, 0);
    assert_eq!(out, b"a\nb\nc\nd\n");
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("4 in, 4 out"), "merge stats: {err}");
}

#[test]
fn count_with_stats() {
    let (out, err, code) = xort_stderr(&["--count", "--stats"], b"b\na\nb\nb\n");
    assert_eq!(code, 0);
    assert_eq!(out, b"      1 a\n      3 b\n");
    let err = String::from_utf8(err).unwrap();
    // 4 lines in, 2 groups out, 2 collapsed.
    assert!(err.contains("4 in, 2 out"), "count stats: {err}");
}

#[test]
fn count_to_output_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("counts.txt");
    let (_, code) = xort(&["--count", "-o", &out.to_string_lossy()], b"b\na\nb\n");
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "      1 a\n      2 b\n"
    );
}

// --- coverage: rich --check diagnostics (diag.rs) ---------------------------

#[test]
fn check_failure_color_always() {
    // A forced-color check failure exercises the ANSI-highlighted report branch.
    let (out, err, code) = xort_stderr(&["-c", "--color=always"], b"b\na\n");
    assert_eq!(code, 1);
    assert!(out.is_empty());
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("not in sorted order"), "stderr: {err}");
    assert!(err.contains('\x1b'), "forced color should emit ANSI: {err}");
}

#[test]
fn check_failure_color_never_is_plain() {
    let (_, err, code) = xort_stderr(&["-c", "--color=never"], b"b\na\n");
    assert_eq!(code, 1);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("not in sorted order"), "stderr: {err}");
    assert!(!err.contains('\x1b'), "color=never must be plain: {err}");
}

// --- coverage: structured-format engine branches ---------------------------

#[test]
fn csv_reverse_and_last_resort() {
    // -k1r reverses the key (cmp_records reverse arm); within an equal key the
    // non-suppressed last-resort compares the whole record.
    let out = run(&["--csv", "-k1r"], "a,2\nb,1\na,1\n");
    assert_eq!(out, "b,1\na,1\na,2\n");
}

#[test]
fn csv_top_and_stats() {
    let (out, err, code) = xort_stderr(&["--csv", "--top", "1", "--stats"], b"c,3\na,1\nb,2\n");
    assert_eq!(code, 0);
    assert_eq!(out, b"a,1\n");
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("3 in, 1 out"), "csv stats: {err}");
}

#[test]
fn csv_to_output_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.csv");
    let (_, code) = xort(&["--csv", "-o", &out.to_string_lossy()], b"b,2\na,1\n");
    assert_eq!(code, 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "a,1\nb,2\n");
}

#[test]
fn json_mixed_scalar_types_order() {
    // Type-ranked ordering: Null < Bool < Number < String.
    let out = run(
        &["--jsonl", "-k", ".v"],
        "{\"v\":\"s\"}\n{\"v\":2}\n{\"v\":true}\n{\"v\":null}\n",
    );
    assert_eq!(
        out,
        "{\"v\":null}\n{\"v\":true}\n{\"v\":2}\n{\"v\":\"s\"}\n"
    );
}

#[test]
fn json_top_stats_and_output_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    let (_, err, code) = xort_stderr(
        &[
            "--json",
            "-k",
            "v",
            "--top",
            "2",
            "--stats",
            "-o",
            &out.to_string_lossy(),
        ],
        b"[{\"v\":3},{\"v\":1},{\"v\":2}]",
    );
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(&out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
    let vs: Vec<i64> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["v"].as_i64().unwrap())
        .collect();
    assert_eq!(vs, vec![1, 2]);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("3 in, 2 out"), "json stats: {err}");
}

#[test]
fn json_non_array_input_errors() {
    // --json (array mode) given a bare object must exit 2.
    let (_, code) = xort(&["--json", "-k", "v"], b"{\"v\":1}");
    assert_eq!(code, 2, "non-array --json input should exit 2");
}
