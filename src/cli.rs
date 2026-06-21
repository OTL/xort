//! Command-line parsing. GNU short flags map 1:1; new features use long-only
//! flags so they never collide with the GNU namespace.

use crate::config::{ColorChoice, Config, Format};
use clap::{ArgAction, ArgGroup, Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "xort",
    version,
    about = "A fast, modern, parallel drop-in replacement for sort",
    // Free up -h and -V (clap's defaults) so they can mean human-numeric and
    // version-sort like GNU; help/version remain available as long flags.
    disable_help_flag = true,
    disable_version_flag = true,
    group = ArgGroup::new("format").args(["csv", "tsv", "json", "jsonl"]),
)]
/// Parsed command-line arguments, convertible into a [`Config`] via
/// [`Cli::into_config`].
pub struct Cli {
    /// Input files ("-" or none means standard input).
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Reverse the result of comparisons.
    #[arg(short = 'r', long = "reverse")]
    pub reverse: bool,

    /// Compare according to leading numeric value.
    #[arg(short = 'n', long = "numeric-sort")]
    pub numeric: bool,

    /// Compare according to general numerical value (floats, exponents).
    #[arg(short = 'g', long = "general-numeric-sort")]
    pub general: bool,

    /// Compare human-readable numbers (e.g. 2K 1G).
    #[arg(short = 'h', long = "human-numeric-sort")]
    pub human: bool,

    /// Natural sort of (version) numbers within text.
    #[arg(short = 'V', long = "version-sort")]
    pub version_sort: bool,

    /// Compare (unknown) < 'JAN' < ... < 'DEC'.
    #[arg(short = 'M', long = "month-sort")]
    pub month: bool,

    /// Output only the first of an equal run of lines.
    #[arg(short = 'u', long = "unique")]
    pub unique: bool,

    /// Stabilize sort by keeping input order of equal lines.
    #[arg(short = 's', long = "stable")]
    pub stable: bool,

    /// Fold lower case to upper case while comparing.
    #[arg(short = 'f', long = "ignore-case")]
    pub fold_case: bool,

    /// Ignore leading blanks in keys.
    #[arg(short = 'b', long = "ignore-leading-blanks")]
    pub ignore_leading_blanks: bool,

    /// Line delimiter is NUL, not newline.
    #[arg(short = 'z', long = "zero-terminated")]
    pub zero_terminated: bool,

    /// Check whether input is sorted; do not sort.
    #[arg(short = 'c', long = "check")]
    pub check: bool,

    /// Merge already-sorted files; do not sort.
    #[arg(short = 'm', long = "merge")]
    pub merge: bool,

    /// Sort via a key; KEYDEF is `F[.C][OPTS][,F[.C][OPTS]]`.
    #[arg(short = 'k', long = "key", value_name = "KEYDEF", action = ArgAction::Append)]
    pub keys: Vec<String>,

    /// Use SEP as the field separator instead of whitespace transitions.
    #[arg(short = 't', long = "field-separator", value_name = "SEP")]
    pub field_separator: Option<String>,

    /// Write result to FILE instead of standard output.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Use at most SIZE for the main memory buffer (e.g. 64M); spill above it.
    #[arg(short = 'S', long = "buffer-size", value_name = "SIZE")]
    pub buffer_size: Option<String>,

    /// Use DIR for temporary spill files (may be repeated).
    #[arg(short = 'T', long = "temporary-directory", value_name = "DIR", action = ArgAction::Append)]
    pub temp_dirs: Vec<PathBuf>,

    /// Change the number of sorts run concurrently to N.
    #[arg(long = "parallel", value_name = "N")]
    pub parallel: Option<usize>,

    /// Emit only the first N lines in sort order (bounded top-N selection).
    #[arg(long = "top", value_name = "N")]
    pub top: Option<usize>,

    /// With -u, prefix each line with its occurrence count (like `uniq -c`).
    #[arg(long = "count")]
    pub count: bool,

    /// Treat the first line as a header: keep it on top, exclude from sorting.
    #[arg(long = "header")]
    pub header: bool,

    /// Treat input as CSV (RFC 4180); sort by column index or name.
    #[arg(long = "csv")]
    pub csv: bool,

    /// Treat input as TSV (tab-separated).
    #[arg(long = "tsv")]
    pub tsv: bool,

    /// Treat input as a JSON array of objects; sort by --key field path.
    #[arg(long = "json")]
    pub json: bool,

    /// Treat input as JSONL/NDJSON; sort by --key field path.
    #[arg(long = "jsonl")]
    pub jsonl: bool,

    /// When to colorize the sort key in output.
    #[arg(long = "color", value_name = "WHEN", default_value = "auto")]
    pub color: ColorArg,

    /// Print a summary (line counts, elapsed time) to stderr.
    #[arg(long = "stats")]
    pub stats: bool,

    /// Generate a shell completion script and exit.
    #[arg(long = "completions", value_name = "SHELL")]
    pub completions: Option<clap_complete::Shell>,

    /// Print the man page (troff) to stdout and exit.
    #[arg(long = "man")]
    pub man: bool,

    /// Print help.
    #[arg(long = "help", action = ArgAction::Help)]
    pub help: Option<bool>,

    /// Print version.
    #[arg(long = "version", action = ArgAction::Version)]
    pub version: Option<bool>,
}

/// The `--color` argument value.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ColorArg {
    /// Colorize only when writing to a terminal (honoring `NO_COLOR`).
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

impl Cli {
    /// Validate the parsed arguments and resolve them into a [`Config`].
    ///
    /// Returns an error message for invalid combinations (e.g. a
    /// multi-character `-t`, or `-z` with `--csv`).
    pub fn into_config(self) -> Result<Config, String> {
        let tab = match &self.field_separator {
            None => None,
            Some(s) => {
                let bytes = s.as_bytes();
                match bytes.len() {
                    1 => Some(bytes[0]),
                    // Allow an escaped tab as a convenience.
                    2 if s == "\\t" => Some(b'\t'),
                    _ => return Err(format!("multi-character tab '{s}'")),
                }
            }
        };

        let format = if self.csv {
            Format::Csv
        } else if self.tsv {
            Format::Tsv
        } else if self.json {
            Format::Json
        } else if self.jsonl {
            Format::Jsonl
        } else {
            Format::Text
        };

        if self.zero_terminated && matches!(format, Format::Csv | Format::Tsv) {
            return Err("-z cannot be combined with --csv/--tsv".into());
        }

        let color = match self.color {
            ColorArg::Auto => ColorChoice::Auto,
            ColorArg::Always => ColorChoice::Always,
            ColorArg::Never => ColorChoice::Never,
        };

        Ok(Config {
            files: self.files,
            output: self.output,
            reverse: self.reverse,
            numeric: self.numeric,
            general: self.general,
            human: self.human,
            version: self.version_sort,
            month: self.month,
            unique: self.unique,
            stable: self.stable,
            fold_case: self.fold_case,
            ignore_leading_blanks: self.ignore_leading_blanks,
            zero_terminated: self.zero_terminated,
            check: self.check,
            merge: self.merge,
            keys: self.keys,
            tab,
            buffer_size: self.buffer_size,
            temp_dirs: self.temp_dirs,
            top: self.top,
            count: self.count,
            header: self.header,
            parallel: self.parallel,
            stats: self.stats,
            format,
            color,
        })
    }
}
