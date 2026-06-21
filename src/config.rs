//! The fully-resolved description of a sort job.

use crate::compare::KeyOpts;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    /// Input files; empty (or `-`) means stdin.
    pub files: Vec<PathBuf>,
    /// `-o`: write to this file instead of stdout.
    pub output: Option<PathBuf>,
    /// `-r`: reverse the sort order.
    pub reverse: bool,
    /// `-n`: numeric sort.
    pub numeric: bool,
    /// `-u`: output only the first line of each equal-key run.
    pub unique: bool,
    /// `-s`: stable; keep input order among equal keys.
    pub stable: bool,
    /// `-f`: fold case.
    pub fold_case: bool,
    /// `-b`: ignore leading blanks.
    pub ignore_leading_blanks: bool,
    /// `-z`: lines are NUL-terminated rather than newline-terminated.
    pub zero_terminated: bool,
    /// `-c`: check whether the input is already sorted; do not sort.
    pub check: bool,
    /// `--top N`: emit only the first N lines in sort order (bounded selection).
    pub top: Option<usize>,
    /// `--parallel N`: thread count (None = auto).
    pub parallel: Option<usize>,
    /// `--stats`: print a summary to stderr after sorting.
    pub stats: bool,
}

impl Config {
    /// The line terminator byte implied by `-z`.
    pub fn terminator(&self) -> u8 {
        if self.zero_terminated {
            0
        } else {
            b'\n'
        }
    }

    /// The key interpretation for the (currently global) sort key.
    pub fn key_opts(&self) -> KeyOpts {
        KeyOpts {
            numeric: self.numeric,
            fold: self.fold_case,
            ignore_blanks: self.ignore_leading_blanks,
        }
    }
}
