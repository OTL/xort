//! Output polish: colorized key highlighting and rich `--check` diagnostics.

use crate::config::{ColorChoice, Config};
use std::io::{self, IsTerminal, Write};

const KEY_ON: &[u8] = b"\x1b[1;33m"; // bold yellow
const KEY_OFF: &[u8] = b"\x1b[0m";

/// Whether to colorize the key in normal (stdout) output: only to a terminal,
/// never to a file (`-o`), honoring `--color` and `NO_COLOR`.
pub fn color_stdout(cfg: &Config) -> bool {
    if cfg.output.is_some() {
        return false;
    }
    match cfg.color {
        ColorChoice::Never => false,
        ColorChoice::Always => true,
        ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal(),
    }
}

fn color_stderr(cfg: &Config) -> bool {
    match cfg.color {
        ColorChoice::Never => false,
        ColorChoice::Always => true,
        ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && io::stderr().is_terminal(),
    }
}

/// Write `line` with the byte range `[s, e)` highlighted as the sort key.
pub fn write_highlighted<W: Write>(
    w: &mut W,
    line: &[u8],
    range: (usize, usize),
    terminator: u8,
) -> io::Result<()> {
    let (s, e) = range;
    let s = s.min(line.len());
    let e = e.clamp(s, line.len());
    w.write_all(&line[..s])?;
    w.write_all(KEY_ON)?;
    w.write_all(&line[s..e])?;
    w.write_all(KEY_OFF)?;
    w.write_all(&line[e..])?;
    w.write_all(std::slice::from_ref(&terminator))?;
    Ok(())
}

/// Rich `--check` failure report: the previous and offending lines with line
/// numbers, and a caret run under the offending key. Far friendlier than GNU's
/// `sort: -:N: disorder: …`.
pub fn report_disorder(
    cfg: &Config,
    prev: &[u8],
    cur: &[u8],
    lineno: usize,
    key_range: (usize, usize),
) {
    let color = color_stderr(cfg);
    let mut err = io::stderr().lock();
    let (s, e) = (key_range.0.min(cur.len()), key_range.1.min(cur.len()));
    let _ = writeln!(
        err,
        "xort: check failed: line {lineno} is not in sorted order"
    );
    let _ = writeln!(
        err,
        "  {:>6} | {}",
        lineno - 1,
        String::from_utf8_lossy(prev)
    );
    let prefix = format!("  {lineno:>6} | ");
    let _ = write!(err, "{prefix}");
    if color {
        let _ = err.write_all(&cur[..s]);
        let _ = err.write_all(KEY_ON);
        let _ = err.write_all(&cur[s..e]);
        let _ = err.write_all(KEY_OFF);
        let _ = err.write_all(&cur[e..]);
        let _ = err.write_all(b"\n");
    } else {
        let _ = writeln!(err, "{}", String::from_utf8_lossy(cur));
    }
    // caret line aligned under the key
    let pad = " ".repeat(prefix.len() + display_width(&cur[..s]));
    let carets = "^".repeat(display_width(&cur[s..e]).max(1));
    let _ = writeln!(err, "{pad}{carets} key not in order here");
}

/// Approximate display width: byte count (ASCII-accurate; good enough for the
/// caret alignment on typical inputs).
fn display_width(bytes: &[u8]) -> usize {
    bytes.len()
}
