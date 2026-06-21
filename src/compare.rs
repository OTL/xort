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

/// Numeric comparison of the leading numbers in `a` and `b` (`-n`).
pub fn numeric_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (na, ia, fa) = split_number(a);
    let (nb, ib, fb) = split_number(b);
    let za = ia.is_empty() && fa.is_empty();
    let zb = ib.is_empty() && fb.is_empty();
    match (za, zb) {
        (true, true) => return Ordering::Equal, // both zero (sign of zero is irrelevant)
        (true, false) => {
            return if nb {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, true) => {
            return if na {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, false) => {}
    }
    if na != nb {
        // negative < positive
        return if na {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    // Fraction digits compare lexically: trailing zeros are stripped, so a
    // shorter slice is the prefix and therefore the smaller fraction.
    let mag = cmp_int(ia, ib).then_with(|| fa.cmp(fb));
    if na {
        mag.reverse()
    } else {
        mag
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
}
