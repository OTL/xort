//! Parallel-parsed LSD radix sort for the integer fast path of global `-n`.
//!
//! When every line's leading `-n` value is an exact integer in `i64` range,
//! [`try_numeric_radix`] sorts by a stable least-significant-digit radix instead
//! of a comparison sort. It produces output **byte-identical** to the comparison
//! path (and thus to GNU `sort -n`): primary order is the numeric value; ties
//! fall back to the whole-line byte comparison unless `-s`/`-u` suppress it
//! (then input order is preserved, which the radix's stability gives for free).
//!
//! Any non-integer key (a fraction, an exponent, an out-of-range magnitude)
//! makes [`try_numeric_radix`] return `None`, and the engine falls back to the
//! arbitrary-precision [`crate::compare::NumericKey`] comparison path.

use crate::config::Config;
use rayon::prelude::*;

/// Map an `i64` to an order-preserving `u64` (ascending): flip the sign bit so
/// negatives sort below non-negatives and the ordering matches numeric order.
#[inline]
fn order_key(v: i64) -> u64 {
    (v as u64) ^ (1u64 << 63)
}

/// Parse the leading integer of `s` as GNU `sort -n` would value it, returning
/// `None` when the value is not an exact `i64` integer — a fractional point
/// adjacent to the number, or a magnitude past `i64` range. A missing number
/// (blank, or non-numeric text like `abc`) is the value `0`, matching GNU.
fn parse_int_key(s: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    // GNU `sort -n` accepts a leading '-' but NOT '+': "+5" parses as the value
    // 0 (the '+' is non-numeric), with the text kept for the byte tie-break.
    let neg = if i < s.len() && s[i] == b'-' {
        i += 1;
        true
    } else {
        false
    };
    let mut val: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        // Accumulate as a negative magnitude so that i64::MIN is representable;
        // overflow returns None so the caller falls back to the exact path.
        val = val.checked_mul(10)?.checked_sub((s[i] - b'0') as i64)?;
        i += 1;
    }
    // A fractional part right after the number is not integer-comparable.
    if i < s.len() && s[i] == b'.' {
        return None;
    }
    // `val` holds the negated magnitude; positive values negate back.
    if neg {
        Some(val)
    } else {
        val.checked_neg()
    }
}

/// Stable LSD radix sort of `recs` by the `u64` key (ascending). Byte passes
/// whose key byte is identical across all records are skipped, so small-range
/// inputs cost only a few passes. Each scatter swaps the working buffer, so the
/// result always lands back in `recs`.
fn radix_sort(recs: &mut Vec<(u64, &[u8])>) {
    let n = recs.len();
    if n <= 1 {
        return;
    }
    let mut dst: Vec<(u64, &[u8])> = recs.clone();
    for shift in (0..64).step_by(8) {
        let mut count = [0usize; 256];
        for &(k, _) in recs.iter() {
            count[((k >> shift) & 0xff) as usize] += 1;
        }
        // Skip a pass whose byte is uniform across all records.
        if count.contains(&n) {
            continue;
        }
        let mut sum = 0usize;
        for c in count.iter_mut() {
            let t = *c;
            *c = sum;
            sum += t;
        }
        for &rec in recs.iter() {
            let b = ((rec.0 >> shift) & 0xff) as usize;
            dst[count[b]] = rec;
            count[b] += 1;
        }
        std::mem::swap(recs, &mut dst);
    }
}

/// Break ties (equal radix keys) by whole-line byte order, matching GNU's
/// last-resort comparison. Equal keys are already contiguous after the radix.
fn tie_break_by_line(recs: &mut [(u64, &[u8])]) {
    let n = recs.len();
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && recs[j].0 == recs[i].0 {
            j += 1;
        }
        if j - i > 1 {
            recs[i..j].sort_unstable_by(|a, b| a.1.cmp(b.1));
        }
        i = j;
    }
}

/// Try to sort `lines` by the integer radix fast path. Returns `None` (so the
/// caller falls back to the comparison path) if any line's `-n` value is not an
/// exact `i64` integer. On success returns the ordered lines and the number of
/// duplicates removed by `-u`.
pub fn try_numeric_radix<'a>(lines: &[&'a [u8]], cfg: &Config) -> Option<(Vec<&'a [u8]>, usize)> {
    // Parse every key up front; bail out (fall back) on the first non-integer.
    let keys: Vec<i64> = lines
        .par_iter()
        .map(|l| parse_int_key(l))
        .collect::<Option<_>>()?;

    let suppress = cfg.stable || cfg.unique;
    let reverse = cfg.reverse;
    // For the suppressed case (`-s`/`-u`), inverting the key lets a single
    // ascending stable radix yield value-descending order while still keeping
    // equal-valued lines in input order.
    let descending = suppress && reverse;

    let mut recs: Vec<(u64, &'a [u8])> = lines
        .iter()
        .zip(&keys)
        .map(|(&l, &k)| {
            let ok = order_key(k);
            (if descending { !ok } else { ok }, l)
        })
        .collect();

    radix_sort(&mut recs);

    if !suppress {
        // Default `-n`: ties resolve by whole-line bytes, then a global reverse
        // flips the entire (value, line) order.
        tie_break_by_line(&mut recs);
        if reverse {
            recs.reverse();
        }
    }

    let mut dups = 0;
    if cfg.unique {
        let before = recs.len();
        recs.dedup_by(|a, b| a.0 == b.0);
        dups = before - recs.len();
    }
    if let Some(n) = cfg.top {
        recs.truncate(n);
    }
    Some((recs.into_iter().map(|(_, l)| l).collect(), dups))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[test]
    fn parse_keys() {
        assert_eq!(parse_int_key(b"5"), Some(5));
        assert_eq!(parse_int_key(b"-3"), Some(-3));
        assert_eq!(parse_int_key(b"+7"), Some(0)); // GNU -n ignores '+': value 0
        assert_eq!(parse_int_key(b"  42"), Some(42));
        assert_eq!(parse_int_key(b"007"), Some(7));
        assert_eq!(parse_int_key(b"5abc"), Some(5)); // trailing junk ignored
        assert_eq!(parse_int_key(b"abc"), Some(0)); // missing number is 0
        assert_eq!(parse_int_key(b""), Some(0));
        assert_eq!(parse_int_key(b"-"), Some(0));
        assert_eq!(parse_int_key(b"0"), Some(0));
        assert_eq!(parse_int_key(b"-0"), Some(0));
        // Fractions and overflow fall back.
        assert_eq!(parse_int_key(b"5.5"), None);
        assert_eq!(parse_int_key(b".5"), None);
        assert_eq!(parse_int_key(b"99999999999999999999"), None);
        // Full i64 range is representable.
        assert_eq!(parse_int_key(b"-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_int_key(b"9223372036854775807"), Some(i64::MAX));
    }

    fn cfg() -> Config {
        // A minimal numeric Config; only the fields radix reads matter here.
        crate::cli::Cli::parse_from(["xort", "-n"])
            .into_config()
            .unwrap()
    }

    fn lines(s: &[&'static str]) -> Vec<&'static [u8]> {
        s.iter().map(|x| x.as_bytes()).collect()
    }

    #[test]
    fn sorts_ints_with_byte_tiebreak() {
        let ls = lines(&["10", "2", "1", "10"]);
        let (out, dups) = try_numeric_radix(&ls, &cfg()).unwrap();
        assert_eq!(out, lines(&["1", "2", "10", "10"]));
        assert_eq!(dups, 0);
    }

    #[test]
    fn tiebreak_matches_whole_line() {
        // Equal value 5; "0000005" < "5" by bytes.
        let ls = lines(&["5", "0000005"]);
        let (out, _) = try_numeric_radix(&ls, &cfg()).unwrap();
        assert_eq!(out, lines(&["0000005", "5"]));
    }

    #[test]
    fn falls_back_on_float() {
        let ls = lines(&["1", "2.5", "3"]);
        assert!(try_numeric_radix(&ls, &cfg()).is_none());
    }

    #[test]
    fn negatives_order_below_positives() {
        let ls = lines(&["-1", "2", "-3", "0"]);
        let (out, _) = try_numeric_radix(&ls, &cfg()).unwrap();
        assert_eq!(out, lines(&["-3", "-1", "0", "2"]));
    }
}
