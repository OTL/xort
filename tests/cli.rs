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
