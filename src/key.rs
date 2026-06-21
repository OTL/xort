//! Sort keys: field specifications (`-k`/`-t`) and the multi-key comparator.
//!
//! Field extraction follows GNU coreutils' `begfield`/`limfield` algorithm so
//! that `-k F.C,G.D` selects the same bytes GNU sort would.

use crate::compare::{self, KeyOpts};
use std::cmp::Ordering;

/// The ordering discipline applied to a key's extracted bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    /// Raw byte comparison (the default).
    #[default]
    Bytes,
    /// `-n`: leading numeric value (arbitrary-precision integer part).
    Numeric,
    /// `-g`: general float value (`f64`, incl. exponents).
    General,
    /// `-h`: human-readable size (1024-based suffixes).
    Human,
    /// `-V`: version/natural ordering.
    Version,
    /// `-M`: month name ordering.
    Month,
}

/// A single sort key. Field/char indices are 1-based, mirroring `-k` syntax.
#[derive(Clone, Debug)]
pub struct KeyDef {
    /// Start field (1-based).
    pub start_field: usize,
    /// Start char within the start field (1-based).
    pub start_char: usize,
    /// End field (1-based); `None` means "to end of line".
    pub end_field: Option<usize>,
    /// End char within the end field (1-based, inclusive); 0 means "whole field".
    pub end_char: usize,
    /// The ordering discipline for this key.
    pub kind: Kind,
    /// Fold case (`f`).
    pub fold: bool,
    /// Skip leading blanks of the start field (`b`).
    pub skip_sblanks: bool,
    /// Skip leading blanks of the end field (`b` after the comma).
    pub skip_eblanks: bool,
    /// Reverse this key's ordering (`r`).
    pub reverse: bool,
}

impl KeyDef {
    /// A whole-line key carrying the global ordering options.
    pub fn whole_line(opts: &KeyOpts, kind: Kind, reverse: bool) -> Self {
        KeyDef {
            start_field: 1,
            start_char: 1,
            end_field: None,
            end_char: 0,
            kind,
            fold: opts.fold,
            skip_sblanks: opts.ignore_blanks,
            skip_eblanks: opts.ignore_blanks,
            reverse,
        }
    }
}

#[inline]
fn is_blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// Global ordering options (set by flags before/without `-k`).
#[derive(Clone, Copy, Debug, Default)]
pub struct GlobalOrder {
    /// Ordering discipline from `-n/-g/-h/-V/-M`.
    pub kind: Kind,
    /// Global `-f`.
    pub fold: bool,
    /// Global `-b`.
    pub ignore_blanks: bool,
    /// Global `-r`.
    pub reverse: bool,
}

/// Map the type letters in a `-k` option string (`nghVM`) to a [`Kind`].
pub fn kind_from_opts(opts: &str) -> Kind {
    if opts.contains('n') {
        Kind::Numeric
    } else if opts.contains('g') {
        Kind::General
    } else if opts.contains('h') {
        Kind::Human
    } else if opts.contains('V') {
        Kind::Version
    } else if opts.contains('M') {
        Kind::Month
    } else {
        Kind::Bytes
    }
}

/// The ordering-option letters xort understands within a `-k` spec.
const KEY_OPT_LETTERS: &str = "bfghMnrV";

/// Parse one position of a `-k` spec: `FIELD[.CHAR][OPTS]`.
/// Returns (field, char, option-letters). `char` is 0 when omitted.
///
/// Unlike GNU's lenient parser, an unknown option letter or a `.` with no
/// following character position is rejected (exit 2), rather than silently
/// ignored.
pub fn parse_pos(s: &str) -> Result<(usize, usize, String), String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return Err(format!("invalid field specification '{s}'"));
    }
    let field: usize = s[..i]
        .parse()
        .map_err(|_| format!("invalid field number in '{s}'"))?;
    if field == 0 {
        return Err(format!("field number is zero in '{s}'"));
    }
    let mut ch = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let cs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == cs {
            return Err(format!("missing character position after '.' in '{s}'"));
        }
        ch = s[cs..i]
            .parse()
            .map_err(|_| format!("invalid character position in '{s}'"))?;
    }
    let opts = &s[i..];
    if let Some(bad) = opts.chars().find(|c| !KEY_OPT_LETTERS.contains(*c)) {
        return Err(format!("invalid ordering option '{bad}' in '{s}'"));
    }
    Ok((field, ch, opts.to_string()))
}

/// Parse a `-k` key specification, applying global inheritance: a key that
/// carries no ordering options of its own inherits all of them from `global`
/// (matching GNU coreutils' all-or-nothing inheritance rule).
pub fn parse_key_spec(spec: &str, global: &GlobalOrder) -> Result<KeyDef, String> {
    let (start, end) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };
    let (sf, sc, sopts) = parse_pos(start)?;
    let (ef, ec, eopts) = match end {
        Some(e) => {
            let (f, c, o) = parse_pos(e)?;
            (Some(f), c, o)
        }
        None => (None, 0, String::new()),
    };

    let mut kd = KeyDef {
        start_field: sf,
        start_char: if sc == 0 { 1 } else { sc },
        end_field: ef,
        end_char: ec, // 0 => whole end field
        kind: Kind::Bytes,
        fold: false,
        skip_sblanks: false,
        skip_eblanks: false,
        reverse: false,
    };

    if sopts.is_empty() && eopts.is_empty() {
        kd.kind = global.kind;
        kd.fold = global.fold;
        kd.reverse = global.reverse;
        kd.skip_sblanks = global.ignore_blanks;
        kd.skip_eblanks = global.ignore_blanks;
    } else {
        let all = format!("{sopts}{eopts}");
        kd.kind = kind_from_opts(&all);
        kd.fold = all.contains('f');
        kd.reverse = all.contains('r');
        kd.skip_sblanks = sopts.contains('b');
        kd.skip_eblanks = eopts.contains('b');
    }
    Ok(kd)
}

/// Index after consuming `groups` whole fields from the start of `line`.
/// In whitespace mode each field is "leading blanks then non-blanks"; in tab
/// mode each field ends at (and excludes) the next delimiter.
fn pos_after_fields(line: &[u8], groups: usize, tab: Option<u8>) -> usize {
    let lim = line.len();
    let mut ptr = 0;
    match tab {
        Some(t) => {
            for _ in 0..groups {
                while ptr < lim && line[ptr] != t {
                    ptr += 1;
                }
                if ptr < lim {
                    ptr += 1; // step over the delimiter
                }
            }
        }
        None => {
            for _ in 0..groups {
                while ptr < lim && is_blank(line[ptr]) {
                    ptr += 1;
                }
                while ptr < lim && !is_blank(line[ptr]) {
                    ptr += 1;
                }
            }
        }
    }
    ptr
}

/// End index of field `n` (1-based): the position just before the n-th
/// delimiter (tab mode) or after the n-th non-blank run (whitespace mode).
fn field_end(line: &[u8], n: usize, tab: Option<u8>) -> usize {
    let lim = line.len();
    match tab {
        Some(t) => {
            // Skip n-1 delimiters, then scan to the n-th delimiter.
            let mut ptr = pos_after_fields(line, n - 1, tab);
            while ptr < lim && line[ptr] != t {
                ptr += 1;
            }
            ptr
        }
        None => pos_after_fields(line, n, tab),
    }
}

/// Extract the key bytes for `key` from `line` (GNU `-k` semantics).
#[inline]
pub fn extract<'a>(line: &'a [u8], key: &KeyDef, tab: Option<u8>) -> &'a [u8] {
    let (beg, end) = extract_range(line, key, tab);
    &line[beg..end]
}

/// The `[start, end)` byte range of `key` within `line` (for highlighting and
/// diagnostics).
pub fn extract_range(line: &[u8], key: &KeyDef, tab: Option<u8>) -> (usize, usize) {
    let lim = line.len();
    // --- start position --------------------------------------------------
    let mut beg = pos_after_fields(line, key.start_field - 1, tab);
    if key.skip_sblanks {
        while beg < lim && is_blank(line[beg]) {
            beg += 1;
        }
    }
    // advance start_char - 1 chars into the field
    let mut sc = key.start_char.saturating_sub(1);
    while beg < lim && sc > 0 {
        beg += 1;
        sc -= 1;
    }
    // --- end position ----------------------------------------------------
    let end = match key.end_field {
        None => lim,
        Some(ef) => {
            if key.end_char == 0 {
                field_end(line, ef, tab) // whole end field
            } else {
                let mut p = pos_after_fields(line, ef - 1, tab);
                if key.skip_eblanks {
                    while p < lim && is_blank(line[p]) {
                        p += 1;
                    }
                }
                let mut ec = key.end_char;
                while p < lim && ec > 0 {
                    p += 1;
                    ec -= 1;
                }
                p
            }
        }
    };
    let end = end.clamp(beg, lim);
    (beg, end)
}

/// A fully-resolved comparison plan: an ordered list of keys plus the global
/// options that govern the last-resort comparison.
#[derive(Clone, Debug)]
pub struct Sorter {
    /// The ordered list of keys (a single whole-line key when no `-k` is given).
    pub keys: Vec<KeyDef>,
    /// Field separator (`-t`); `None` means whitespace-transition fields.
    pub tab: Option<u8>,
    /// Global `-r`, applied to the whole-line last-resort comparison.
    pub global_reverse: bool,
    /// Suppress the whole-line last-resort comparison (`-s` or `-u`).
    pub suppress_last_resort: bool,
}

impl Sorter {
    /// Compare two whole lines under this plan.
    #[inline]
    pub fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
        let mut key_ord = Ordering::Equal;
        for key in &self.keys {
            let ka = extract(a, key, self.tab);
            let kb = extract(b, key, self.tab);
            let mut o = compare::compare_kind(ka, kb, key.kind, key.fold);
            if key.reverse {
                o = o.reverse();
            }
            if o != Ordering::Equal {
                key_ord = o;
                break;
            }
        }
        self.finish(key_ord, a, b)
    }

    /// Apply the whole-line last-resort tie-break on top of a key-level
    /// ordering: when the keys compare equal and last-resort is not suppressed
    /// (`-s`/`-u`), order by the raw lines under the global reverse flag. This
    /// is the single home for that contract — the in-memory decorate-sort path
    /// calls it too, so its semantics can never drift from `compare`.
    #[inline]
    pub fn finish(&self, key_ord: Ordering, a: &[u8], b: &[u8]) -> Ordering {
        if key_ord != Ordering::Equal {
            return key_ord;
        }
        if self.suppress_last_resort {
            return Ordering::Equal;
        }
        let o = a.cmp(b);
        if self.global_reverse {
            o.reverse()
        } else {
            o
        }
    }

    /// Equality under the keys alone (for `-u` / `-c`), ignoring last-resort.
    #[inline]
    pub fn key_equal(&self, a: &[u8], b: &[u8]) -> bool {
        for key in &self.keys {
            let ka = extract(a, key, self.tab);
            let kb = extract(b, key, self.tab);
            if compare::compare_kind(ka, kb, key.kind, key.fold) != Ordering::Equal {
                return false;
            }
        }
        true
    }

    /// The byte range of the first (primary) key within `line`, for output
    /// highlighting.
    pub fn first_key_range(&self, line: &[u8]) -> (usize, usize) {
        match self.keys.first() {
            Some(k) => extract_range(line, k, self.tab),
            None => (0, line.len()),
        }
    }

    /// The range (within `b`) of the key that put `b` out of order relative to
    /// `a`, for `--check` diagnostics. Falls back to the whole line.
    pub fn breaking_key_range(&self, a: &[u8], b: &[u8]) -> (usize, usize) {
        for key in &self.keys {
            let mut o = compare::compare_kind(
                extract(a, key, self.tab),
                extract(b, key, self.tab),
                key.kind,
                key.fold,
            );
            if key.reverse {
                o = o.reverse();
            }
            if o != Ordering::Equal {
                return extract_range(b, key, self.tab);
            }
        }
        (0, b.len())
    }

    /// Ordering for `-c` checking: keys plus per-key reverse, no last-resort.
    #[inline]
    pub fn check_compare(&self, a: &[u8], b: &[u8]) -> Ordering {
        for key in &self.keys {
            let ka = extract(a, key, self.tab);
            let kb = extract(b, key, self.tab);
            let mut o = compare::compare_kind(ka, kb, key.kind, key.fold);
            if key.reverse {
                o = o.reverse();
            }
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sf: usize, sc: usize, ef: Option<usize>, ec: usize) -> KeyDef {
        KeyDef {
            start_field: sf,
            start_char: sc,
            end_field: ef,
            end_char: ec,
            kind: Kind::Bytes,
            fold: false,
            skip_sblanks: false,
            skip_eblanks: false,
            reverse: false,
        }
    }

    #[test]
    fn extract_field_tab() {
        // "a:b:c", -t: -k2,2  => "b"
        let line = b"a:b:c";
        assert_eq!(extract(line, &key(2, 1, Some(2), 0), Some(b':')), b"b");
    }

    #[test]
    fn extract_field_range_tab() {
        // -k1,2 => "a:b"
        let line = b"a:b:c";
        assert_eq!(extract(line, &key(1, 1, Some(2), 0), Some(b':')), b"a:b");
    }

    #[test]
    fn extract_field_to_eol() {
        // -k2 => "b:c"
        let line = b"a:b:c";
        assert_eq!(extract(line, &key(2, 1, None, 0), Some(b':')), b"b:c");
    }

    #[test]
    fn extract_whitespace_default() {
        // default blanks; -k2,2 on "foo   bar baz" => leading blanks included
        let line = b"foo   bar baz";
        assert_eq!(extract(line, &key(2, 1, Some(2), 0), None), b"   bar");
    }

    #[test]
    fn extract_start_char() {
        // -k1.3 (3rd char of field 1) on "hello" => "llo"
        let line = b"hello";
        assert_eq!(extract(line, &key(1, 3, None, 0), None), b"llo");
    }

    #[test]
    fn extract_char_range() {
        // -k1.2,1.3 on "abcde" => "bc"
        let line = b"abcde";
        assert_eq!(extract(line, &key(1, 2, Some(1), 3), None), b"bc");
    }
}
