//! The unified comparator: how key bytes are ordered.
//!
//! Milestone 1 supports a single global key (the whole line) with the global
//! type/transform modifiers `-n` (numeric), `-f` (fold case) and `-b` (ignore
//! leading blanks). Per-field `-k`/`-t` keys arrive in milestone 2 and will
//! build on `compare_key`.

use std::cmp::Ordering;

/// How to interpret and order a key's bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyOpts {
    /// `-n`: compare the leading numeric value.
    pub numeric: bool,
    /// `-f`: fold lower case to upper case before comparing.
    pub fold: bool,
    /// `-b`: ignore leading blanks of the key.
    pub ignore_blanks: bool,
}

#[inline]
fn skip_blanks(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    &s[i..]
}

#[inline]
fn strip_leading_zeros(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && s[i] == b'0' {
        i += 1;
    }
    &s[i..]
}

#[inline]
fn strip_trailing_zeros(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 && s[end - 1] == b'0' {
        end -= 1;
    }
    &s[..end]
}

/// Decompose a leading number into (negative, integer-digits, fraction-digits),
/// with insignificant zeros removed so the digit slices can be compared directly.
/// Non-numeric input yields an empty magnitude, i.e. the value zero (matching
/// GNU `sort -n`, which treats a missing number as 0).
fn split_number(s: &[u8]) -> (bool, &[u8], &[u8]) {
    let s = skip_blanks(s);
    let mut i = 0;
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let int_start = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    let int = &s[int_start..i];
    let mut frac: &[u8] = &[];
    if i < s.len() && s[i] == b'.' {
        i += 1;
        let fs = i;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
        frac = &s[fs..i];
    }
    (neg, strip_leading_zeros(int), strip_trailing_zeros(frac))
}

/// Compare integer-digit slices that carry no leading zeros: longer is larger,
/// otherwise lexical order coincides with numeric order.
#[inline]
fn cmp_int(a: &[u8], b: &[u8]) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// A leading number decomposed once, so it can be compared many times without
/// re-parsing. The digit slices borrow the source line. This is the key to the
/// decorate-sort-undecorate path: `O(n)` parses instead of `O(n log n)`.
#[derive(Clone, Copy, Debug)]
pub struct NumericKey<'a> {
    neg: bool,
    zero: bool,
    int: &'a [u8],
    frac: &'a [u8],
}

impl<'a> NumericKey<'a> {
    /// Parse the leading number of `s` once, borrowing its digit slices.
    #[inline]
    pub fn parse(s: &'a [u8]) -> Self {
        let (neg, int, frac) = split_number(s);
        NumericKey {
            neg,
            zero: int.is_empty() && frac.is_empty(),
            int,
            frac,
        }
    }
}

impl Ord for NumericKey<'_> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.zero, other.zero) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.neg {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, true) => {
                return if self.neg {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, false) => {}
        }
        if self.neg != other.neg {
            return if self.neg {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        // Fraction digits compare lexically: trailing zeros are stripped, so a
        // shorter slice is the prefix and therefore the smaller fraction.
        let mag = cmp_int(self.int, other.int).then_with(|| self.frac.cmp(other.frac));
        if self.neg {
            mag.reverse()
        } else {
            mag
        }
    }
}

impl PartialOrd for NumericKey<'_> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for NumericKey<'_> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NumericKey<'_> {}

/// Numeric comparison of the leading numbers in `a` and `b` (`-n`). Used on the
/// non-decorated paths (`-c`, `-u` equality); the sort itself decorates first.
#[inline]
pub fn numeric_cmp(a: &[u8], b: &[u8]) -> Ordering {
    NumericKey::parse(a).cmp(&NumericKey::parse(b))
}

/// General numeric comparison (`-g`): parse a leading float (incl. exponent)
/// and compare as `f64`. Unparseable input is treated as the smallest value
/// (sorts first), which matches GNU's handling of non-numbers under `-g`.
pub fn general_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let fa = parse_f64(a);
    let fb = parse_f64(b);
    match (fa, fb) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn parse_f64(s: &[u8]) -> Option<f64> {
    let s = trim_blanks(s);
    // Find the longest leading prefix that parses as a float.
    let mut end = 0;
    let bytes = s;
    // optional sign
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    let mut seen_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        seen_digit = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            seen_digit = true;
        }
    }
    if seen_digit && end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let mut e = end + 1;
        if e < bytes.len() && (bytes[e] == b'+' || bytes[e] == b'-') {
            e += 1;
        }
        let mut exp_digit = false;
        while e < bytes.len() && bytes[e].is_ascii_digit() {
            e += 1;
            exp_digit = true;
        }
        if exp_digit {
            end = e;
        }
    }
    if !seen_digit {
        return None;
    }
    std::str::from_utf8(&bytes[..end]).ok()?.parse::<f64>().ok()
}

#[inline]
fn trim_blanks(s: &[u8]) -> &[u8] {
    skip_blanks(s)
}

/// Human-readable size comparison (`-h`): a leading number with an optional
/// SI/IEC suffix (K, M, G, T, P, E, Z, Y) scaled in powers of 1024, matching
/// GNU `sort -h`.
pub fn human_cmp(a: &[u8], b: &[u8]) -> Ordering {
    human_value(a)
        .partial_cmp(&human_value(b))
        .unwrap_or(Ordering::Equal)
}

fn human_value(s: &[u8]) -> f64 {
    let s = trim_blanks(s);
    let base = match parse_f64(s) {
        Some(v) => v,
        None => return 0.0,
    };
    // Locate the suffix character after the numeric prefix.
    let mut i = 0;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'.') {
        i += 1;
    }
    let exp = match s.get(i) {
        Some(b'K') | Some(b'k') => 1,
        Some(b'M') => 2,
        Some(b'G') => 3,
        Some(b'T') => 4,
        Some(b'P') => 5,
        Some(b'E') => 6,
        Some(b'Z') => 7,
        Some(b'Y') => 8,
        _ => 0,
    };
    base * 1024f64.powi(exp)
}

/// Month comparison (`-M`): unknown < JAN < ... < DEC.
pub fn month_cmp(a: &[u8], b: &[u8]) -> Ordering {
    month_num(a).cmp(&month_num(b))
}

fn month_num(s: &[u8]) -> u8 {
    let s = trim_blanks(s);
    if s.len() < 3 {
        return 0;
    }
    let m = [
        s[0].to_ascii_uppercase(),
        s[1].to_ascii_uppercase(),
        s[2].to_ascii_uppercase(),
    ];
    match &m {
        b"JAN" => 1,
        b"FEB" => 2,
        b"MAR" => 3,
        b"APR" => 4,
        b"MAY" => 5,
        b"JUN" => 6,
        b"JUL" => 7,
        b"AUG" => 8,
        b"SEP" => 9,
        b"OCT" => 10,
        b"NOV" => 11,
        b"DEC" => 12,
        _ => 0,
    }
}

/// Version comparison (`-V`): natural ordering of mixed letter/number runs,
/// so `v2 < v10` and `1.9 < 1.10`. A pragmatic `strverscmp`-style algorithm.
pub fn version_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (ca, cb) = (a[i], b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            // Compare two digit runs numerically (ignoring leading zeros).
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let da = strip_leading_zeros(&a[si..i]);
            let db = strip_leading_zeros(&b[sj..j]);
            match cmp_int(da, db) {
                Ordering::Equal => {}
                other => return other,
            }
        } else {
            match ca.cmp(&cb) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                other => return other,
            }
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Dispatch a key comparison to the right discipline. `fold` applies to byte
/// comparison only (matching GNU, where `-f` affects ordering of text keys).
#[inline]
pub fn compare_kind(a: &[u8], b: &[u8], kind: crate::key::Kind, fold: bool) -> Ordering {
    use crate::key::Kind;
    match kind {
        Kind::Numeric => numeric_cmp(a, b),
        Kind::General => general_cmp(a, b),
        Kind::Human => human_cmp(a, b),
        Kind::Version => version_cmp(a, b),
        Kind::Month => month_cmp(a, b),
        Kind::Bytes => {
            if fold {
                fold_cmp(a, b)
            } else {
                a.cmp(b)
            }
        }
    }
}

/// Case-folded byte comparison (`-f`): ASCII lower case folds to upper case.
fn fold_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        let x = a[i].to_ascii_uppercase();
        let y = b[i].to_ascii_uppercase();
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.len().cmp(&b.len())
}

/// Compare two keys according to `opts`. This is the ordering used to decide
/// equality for `-u` (unique) and `-c` (check); it does *not* apply the
/// whole-line last-resort tie-break.
#[inline]
pub fn compare_key(a: &[u8], b: &[u8], opts: &KeyOpts) -> Ordering {
    let (a, b) = if opts.ignore_blanks {
        (skip_blanks(a), skip_blanks(b))
    } else {
        (a, b)
    };
    if opts.numeric {
        numeric_cmp(a, b)
    } else if opts.fold {
        fold_cmp(a, b)
    } else {
        a.cmp(b)
    }
}

/// The full line comparison used by the sort itself.
///
/// When not `stable`, GNU breaks key ties with a raw byte comparison of the
/// whole line (the "last-resort" comparison), producing a total order. When
/// `stable` (`-s`), ties are reported as `Equal` so a stable sort preserves
/// input order. `-r` reverses the final result (keys *and* last-resort), which
/// matches GNU `sort -r`.
#[inline]
pub fn full_compare(a: &[u8], b: &[u8], opts: &KeyOpts, reverse: bool, stable: bool) -> Ordering {
    let mut ord = compare_key(a, b, opts);
    if ord == Ordering::Equal && !stable {
        ord = a.cmp(b);
    }
    if reverse {
        ord.reverse()
    } else {
        ord
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    fn n(a: &str, b: &str) -> Ordering {
        numeric_cmp(a.as_bytes(), b.as_bytes())
    }

    #[test]
    fn numeric_basic() {
        assert_eq!(n("2", "10"), Less);
        assert_eq!(n("10", "2"), Greater);
        assert_eq!(n("10", "10"), Equal);
        assert_eq!(n("-5", "3"), Less);
        assert_eq!(n("-5", "-3"), Less); // -5 < -3 (larger magnitude is smaller)
        assert_eq!(n("0", "0"), Equal);
        assert_eq!(n("-0", "0"), Equal);
    }

    #[test]
    fn numeric_fractions() {
        assert_eq!(n("1.5", "1.45"), Greater);
        assert_eq!(n("1.5", "1.50"), Equal);
        assert_eq!(n("1.5", "1.51"), Less);
        assert_eq!(n("0.5", "1"), Less);
        assert_eq!(n("1.5", "1"), Greater);
    }

    #[test]
    fn numeric_nonnumeric_is_zero() {
        assert_eq!(n("abc", "0"), Equal);
        assert_eq!(n("abc", "5"), Less);
        assert_eq!(n("abc", "-5"), Greater);
        assert_eq!(n("  42", "42"), Equal); // leading blanks skipped
    }

    #[test]
    fn fold_case() {
        let o = KeyOpts {
            fold: true,
            ..Default::default()
        };
        assert_eq!(compare_key(b"apple", b"APPLE", &o), Equal);
        assert_eq!(compare_key(b"Apple", b"banana", &o), Less);
    }

    #[test]
    fn last_resort_breaks_fold_ties() {
        let o = KeyOpts {
            fold: true,
            ..Default::default()
        };
        // "Apple" and "apple" fold-equal; last resort: 'A'(65) < 'a'(97).
        assert_eq!(full_compare(b"Apple", b"apple", &o, false, false), Less);
        // stable: tie preserved.
        assert_eq!(full_compare(b"Apple", b"apple", &o, false, true), Equal);
    }

    #[test]
    fn general_floats() {
        assert_eq!(general_cmp(b"1e3", b"50"), Greater);
        assert_eq!(general_cmp(b"2.5e1", b"25"), Equal);
        assert_eq!(general_cmp(b"-1.5", b"-1.4"), Less);
        // unparseable sorts before any number
        assert_eq!(general_cmp(b"x", b"0"), Less);
        assert_eq!(general_cmp(b"x", b"y"), Equal);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_cmp(b"1K", b"500"), Greater);
        assert_eq!(human_cmp(b"1M", b"1024K"), Equal);
        assert_eq!(human_cmp(b"2G", b"1T"), Less);
        assert_eq!(human_cmp(b"1.5K", b"1500"), Greater);
        assert_eq!(human_cmp(b"junk", b"0"), Equal);
    }

    #[test]
    fn versions() {
        assert_eq!(version_cmp(b"v2", b"v10"), Less);
        assert_eq!(version_cmp(b"1.9", b"1.10"), Less);
        assert_eq!(version_cmp(b"1.0", b"1.0"), Equal);
        assert_eq!(version_cmp(b"a1", b"a1b"), Less);
        assert_eq!(version_cmp(b"file01", b"file1"), Equal); // leading zeros ignored
    }

    #[test]
    fn months() {
        assert_eq!(month_cmp(b"JAN", b"FEB"), Less);
        assert_eq!(month_cmp(b"dec", b"jan"), Greater);
        assert_eq!(month_cmp(b"Mar 1", b"March"), Equal); // first 3 letters
        assert_eq!(month_cmp(b"???", b"JAN"), Less); // unknown < JAN
    }

    #[test]
    fn compare_kind_dispatch() {
        use crate::key::Kind;
        assert_eq!(compare_kind(b"2", b"10", Kind::Numeric, false), Less);
        assert_eq!(compare_kind(b"v2", b"v10", Kind::Version, false), Less);
        assert_eq!(compare_kind(b"1K", b"2K", Kind::Human, false), Less);
        assert_eq!(compare_kind(b"JAN", b"FEB", Kind::Month, false), Less);
        assert_eq!(compare_kind(b"abc", b"ABC", Kind::Bytes, true), Equal);
        assert_eq!(compare_kind(b"abc", b"abd", Kind::Bytes, false), Less);
    }
}
