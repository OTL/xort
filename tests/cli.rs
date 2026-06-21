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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin)
        .expect("write stdin");
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
