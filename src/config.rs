//! The fully-resolved description of a sort job.

use crate::compare::KeyOpts;
use crate::key::{self, GlobalOrder, KeyDef, Kind, Sorter};
use std::path::PathBuf;

/// The fully-resolved description of a sort job, produced from [`crate::cli::Cli`].
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
    /// `-g`: general numeric (float) sort.
    pub general: bool,
    /// `-h`: human-readable size sort.
    pub human: bool,
    /// `-V`: version (natural) sort.
    pub version: bool,
    /// `-M`: month sort.
    pub month: bool,
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
    /// `-m`: merge already-sorted inputs.
    pub merge: bool,
    /// `-k`: raw key specifications, in order.
    pub keys: Vec<String>,
    /// `-t`: field separator byte.
    pub tab: Option<u8>,
    /// `-S`: in-memory buffer size (e.g. "64M"); triggers external sort above it.
    pub buffer_size: Option<String>,
    /// `-T`: temp directories for external sort spill files.
    pub temp_dirs: Vec<PathBuf>,
    /// `--top N`: emit only the first N lines in sort order (bounded selection).
    pub top: Option<usize>,
    /// `--count`: with `-u`, prefix each line with its occurrence count.
    pub count: bool,
    /// `--header`: treat the first line as a header (kept on top, not sorted).
    pub header: bool,
    /// `--parallel N`: thread count (None = auto).
    pub parallel: Option<usize>,
    /// `--stats`: print a summary to stderr after sorting.
    pub stats: bool,
    /// Input/output structured format.
    pub format: Format,
    /// `--color`: when to colorize the sort key in output.
    pub color: ColorChoice,
}

/// Structured-input format selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    /// Plain text lines (default).
    #[default]
    Text,
    /// Comma-separated values (RFC 4180).
    Csv,
    /// Tab-separated values.
    Tsv,
    /// A single JSON array of values.
    Json,
    /// JSON Lines / NDJSON (one value per line).
    Jsonl,
}

/// When to colorize the sort key in output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Colorize only when writing to a terminal (honoring `NO_COLOR`).
    #[default]
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
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

    /// Whether the global key is a plain whole-line byte or numeric key with no
    /// `-k` fields — the case the fast milestone-1 paths handle directly.
    pub fn is_simple_global(&self) -> bool {
        self.keys.is_empty()
            && !self.general
            && !self.human
            && !self.version
            && !self.month
            && self.format == Format::Text
    }

    /// The key interpretation for the simple global path.
    pub fn key_opts(&self) -> KeyOpts {
        KeyOpts {
            numeric: self.numeric,
            fold: self.fold_case,
            ignore_blanks: self.ignore_leading_blanks,
        }
    }

    /// The global ordering kind selected by the type flags (precedence matches
    /// the order GNU documents; only one is normally given).
    pub fn global_kind(&self) -> Kind {
        if self.numeric {
            Kind::Numeric
        } else if self.general {
            Kind::General
        } else if self.human {
            Kind::Human
        } else if self.version {
            Kind::Version
        } else if self.month {
            Kind::Month
        } else {
            Kind::Bytes
        }
    }

    fn global_order(&self) -> GlobalOrder {
        GlobalOrder {
            kind: self.global_kind(),
            fold: self.fold_case,
            ignore_blanks: self.ignore_leading_blanks,
            reverse: self.reverse,
        }
    }

    /// Build the multi-key comparison plan used by the general sort path.
    pub fn build_sorter(&self) -> Result<Sorter, String> {
        let global = self.global_order();
        let keys = if self.keys.is_empty() {
            vec![KeyDef::whole_line(
                &self.key_opts(),
                global.kind,
                global.reverse,
            )]
        } else {
            self.keys
                .iter()
                .map(|spec| key::parse_key_spec(spec, &global))
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Sorter {
            keys,
            tab: self.tab,
            global_reverse: self.reverse,
            suppress_last_resort: self.stable || self.unique,
        })
    }
}
