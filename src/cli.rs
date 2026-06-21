//! Command-line parsing. GNU short flags map 1:1; new features use long-only
//! flags so they never collide with the GNU namespace.

use crate::config::Config;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fsort",
    version,
    about = "A fast, modern, parallel drop-in replacement for sort",
    // GNU sort uses -h and -V for its own options; keep them free for later
    // milestones by not auto-binding the short forms here.
    disable_help_flag = false
)]
pub struct Cli {
    /// Input files (\"-\" or none means standard input).
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Reverse the result of comparisons.
    #[arg(short = 'r', long = "reverse")]
    pub reverse: bool,

    /// Compare according to leading numeric value.
    #[arg(short = 'n', long = "numeric-sort")]
    pub numeric: bool,

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

    /// Write result to FILE instead of standard output.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Change the number of sorts run concurrently to N.
    #[arg(long = "parallel", value_name = "N")]
    pub parallel: Option<usize>,

    /// Emit only the first N lines in sort order (bounded top-N selection).
    #[arg(long = "top", value_name = "N")]
    pub top: Option<usize>,

    /// Print a summary (line counts, elapsed time) to stderr.
    #[arg(long = "stats")]
    pub stats: bool,
}

impl Cli {
    pub fn into_config(self) -> Config {
        Config {
            files: self.files,
            output: self.output,
            reverse: self.reverse,
            numeric: self.numeric,
            unique: self.unique,
            stable: self.stable,
            fold_case: self.fold_case,
            ignore_leading_blanks: self.ignore_leading_blanks,
            zero_terminated: self.zero_terminated,
            check: self.check,
            top: self.top,
            parallel: self.parallel,
            stats: self.stats,
        }
    }
}
