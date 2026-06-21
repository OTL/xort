//! Black-box integration tests driving the compiled `fsort` binary.

use std::io::Write;
use std::process::{Command, Stdio};

fn fsort(args: &[&str], stdin: &[u8]) -> (Vec<u8>, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fsort"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fsort");
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
    let (out, code) = fsort(args, stdin.as_bytes());
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
    let (out, code) = fsort(&["-z"], b"b\0a\0c\0");
    assert_eq!(code, 0);
    assert_eq!(out, b"a\0b\0c\0");
}

#[test]
fn check_sorted_exit_codes() {
    let (_, ok) = fsort(&["-c"], b"a\nb\nc\n");
    assert_eq!(ok, 0);
    let (_, bad) = fsort(&["-c"], b"a\nc\nb\n");
    assert_eq!(bad, 1);
}

#[test]
fn multiple_files_merge() {
    // Two files, the first lacking a trailing newline, must not merge lines.
    let dir = std::env::temp_dir();
    let f1 = dir.join("fsort_it_f1.txt");
    let f2 = dir.join("fsort_it_f2.txt");
    std::fs::write(&f1, b"banana\napple").unwrap(); // no trailing newline
    std::fs::write(&f2, b"cherry\n").unwrap();
    let out = run(&[f1.to_str().unwrap(), f2.to_str().unwrap()], "");
    assert_eq!(out, "apple\nbanana\ncherry\n");
    let _ = std::fs::remove_file(f1);
    let _ = std::fs::remove_file(f2);
}
