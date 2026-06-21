//! Structured-input sorting: CSV/TSV and JSON/JSONL.
//!
//! These paths resolve *what the key is* (a CSV column by index/name, or a JSON
//! field path) and then order the records. Plain-text sorting stays in
//! `engine`; this module owns only the structured formats.

use crate::compare::compare_kind;
use crate::config::{Config, Format};
use crate::engine::{Outcome, Stats};
use crate::input::read_all;
use crate::key::{self, Kind};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::io::{self, BufWriter, Write};
use std::time::Instant;

fn invalid(e: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e)
}

/// Sort a structured input (CSV/TSV/JSON/JSONL) per `cfg`, writing the result.
pub fn run_structured(cfg: &Config, start: Instant) -> io::Result<Outcome> {
    match cfg.format {
        Format::Csv | Format::Tsv => run_csv(cfg, start),
        Format::Json => run_json(cfg, start, false),
        Format::Jsonl => run_json(cfg, start, true),
        Format::Text => unreachable!("text is handled by the engine"),
    }
}

// ---------------------------------------------------------------------------
// CSV / TSV
// ---------------------------------------------------------------------------

struct ColKey {
    col: Option<usize>, // 0-based; None => whole record
    kind: Kind,
    fold: bool,
    reverse: bool,
}

fn parse_csv_keys(cfg: &Config, headers: Option<&csv::ByteRecord>) -> Result<Vec<ColKey>, String> {
    if cfg.keys.is_empty() {
        return Ok(vec![ColKey {
            col: None,
            kind: cfg.global_kind(),
            fold: cfg.fold_case,
            reverse: cfg.reverse,
        }]);
    }
    cfg.keys
        .iter()
        .map(|spec| {
            if spec.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) {
                // Numeric column index with the same `F[.C][OPTS][,F[.C][OPTS]]`
                // grammar as text `-k`. A CSV column is atomic, so character
                // positions and multi-column ranges are unsupported; the range
                // form is accepted only as `-kN,Nopts` so its options are not
                // silently dropped.
                let (start, end) = match spec.split_once(',') {
                    Some((s, e)) => (s, Some(e)),
                    None => (spec.as_str(), None),
                };
                let (sf, sc, sopts) = key::parse_pos(start)?;
                let (ec, eopts) = match end {
                    Some(e) => {
                        let (ef, ec, eo) = key::parse_pos(e)?;
                        if ef != sf {
                            return Err(format!(
                                "multi-column key ranges are not supported for CSV/TSV: '{spec}'"
                            ));
                        }
                        (ec, eo)
                    }
                    None => (0, String::new()),
                };
                if sc != 0 || ec != 0 {
                    return Err(format!(
                        "character positions are not supported for CSV/TSV columns: '{spec}'"
                    ));
                }
                let opts = format!("{sopts}{eopts}");
                // All-or-nothing inheritance, mirroring text `-k`: a key with no
                // inline options of its own takes the global type/fold/reverse.
                let (kind, fold, reverse) = if opts.is_empty() {
                    (cfg.global_kind(), cfg.fold_case, cfg.reverse)
                } else {
                    (
                        key::kind_from_opts(&opts),
                        opts.contains('f'),
                        opts.contains('r'),
                    )
                };
                Ok(ColKey {
                    col: Some(sf - 1),
                    kind,
                    fold,
                    reverse,
                })
            } else {
                // column name; type/order come from global flags
                let headers = headers
                    .ok_or_else(|| format!("column name '{spec}' requires --header for CSV/TSV"))?;
                let col = headers
                    .iter()
                    .position(|h| h == spec.as_bytes())
                    .ok_or_else(|| {
                        let names: Vec<String> = headers
                            .iter()
                            .map(|h| String::from_utf8_lossy(h).into_owned())
                            .collect();
                        format!("no column named '{spec}'; available: {}", names.join(", "))
                    })?;
                Ok(ColKey {
                    col: Some(col),
                    kind: cfg.global_kind(),
                    fold: cfg.fold_case,
                    reverse: cfg.reverse,
                })
            }
        })
        .collect()
}

#[inline]
fn field(rec: &csv::ByteRecord, col: Option<usize>) -> &[u8] {
    match col {
        None => rec.as_slice(),
        Some(i) => rec.get(i).unwrap_or(b""),
    }
}

fn cmp_records(
    a: &csv::ByteRecord,
    b: &csv::ByteRecord,
    keys: &[ColKey],
    suppress: bool,
) -> Ordering {
    for k in keys {
        let mut o = compare_kind(field(a, k.col), field(b, k.col), k.kind, k.fold);
        if k.reverse {
            o = o.reverse();
        }
        if o != Ordering::Equal {
            return o;
        }
    }
    if suppress {
        Ordering::Equal
    } else {
        a.as_slice().cmp(b.as_slice())
    }
}

fn keys_equal(a: &csv::ByteRecord, b: &csv::ByteRecord, keys: &[ColKey]) -> bool {
    keys.iter()
        .all(|k| compare_kind(field(a, k.col), field(b, k.col), k.kind, k.fold) == Ordering::Equal)
}

fn run_csv(cfg: &Config, start: Instant) -> io::Result<Outcome> {
    let delim = cfg.tab.unwrap_or(match cfg.format {
        Format::Tsv => b'\t',
        _ => b',',
    });
    let data = read_all(&cfg.files, b'\n')?;
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(cfg.header)
        .flexible(true)
        .from_reader(&data[..]);

    let headers = if cfg.header {
        Some(rdr.byte_headers().map_err(csv_err)?.clone())
    } else {
        None
    };
    let keys = parse_csv_keys(cfg, headers.as_ref()).map_err(invalid)?;
    let mut records: Vec<csv::ByteRecord> = Vec::new();
    for r in rdr.byte_records() {
        records.push(r.map_err(csv_err)?);
    }
    let lines_in = records.len();

    let suppress = cfg.stable || cfg.unique;
    if suppress {
        records.par_sort_by(|a, b| cmp_records(a, b, &keys, true));
    } else {
        records.par_sort_unstable_by(|a, b| cmp_records(a, b, &keys, false));
    }

    if cfg.unique {
        records.dedup_by(|a, b| keys_equal(a, b, &keys));
    }
    if let Some(n) = cfg.top {
        records.truncate(n);
    }

    let sink: Box<dyn Write> = match &cfg.output {
        Some(p) => crate::compress::create_output(p)?,
        None => Box::new(io::stdout().lock()),
    };
    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delim)
        .from_writer(BufWriter::new(sink));
    if let Some(h) = &headers {
        wtr.write_byte_record(h).map_err(csv_err)?;
    }
    for rec in &records {
        wtr.write_byte_record(rec).map_err(csv_err)?;
    }
    wtr.flush()?;

    let lines_out = records.len();
    let stats = cfg.stats.then(|| Stats {
        lines_in,
        lines_out,
        duplicates_removed: lines_in.saturating_sub(lines_out),
        chunks: None,
        elapsed_secs: start.elapsed().as_secs_f64(),
    });
    Ok(Outcome {
        exit_code: 0,
        stats,
    })
}

fn csv_err(e: csv::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

// ---------------------------------------------------------------------------
// JSON / JSONL
// ---------------------------------------------------------------------------

/// A comparable JSON scalar. Ordering: Null < Bool < Number < String < Other,
/// numbers numerically and strings lexically.
#[derive(PartialEq)]
enum JKey {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Other(String),
}

impl JKey {
    fn rank(&self) -> u8 {
        match self {
            JKey::Null => 0,
            JKey::Bool(_) => 1,
            JKey::Num(_) => 2,
            JKey::Str(_) => 3,
            JKey::Other(_) => 4,
        }
    }
    fn cmp(&self, other: &JKey) -> Ordering {
        match (self, other) {
            (JKey::Bool(a), JKey::Bool(b)) => a.cmp(b),
            (JKey::Num(a), JKey::Num(b)) => a.total_cmp(b),
            (JKey::Str(a), JKey::Str(b)) => a.cmp(b),
            (JKey::Other(a), JKey::Other(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

fn json_key(value: &serde_json::Value, path: &[String]) -> JKey {
    let mut cur = value;
    for seg in path {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => return JKey::Null,
        }
    }
    use serde_json::Value::*;
    match cur {
        Null => JKey::Null,
        Bool(b) => JKey::Bool(*b),
        Number(n) => JKey::Num(n.as_f64().unwrap_or(f64::NAN)),
        String(s) => JKey::Str(s.clone()),
        other => JKey::Other(other.to_string()),
    }
}

fn parse_paths(cfg: &Config) -> Vec<Vec<String>> {
    let paths: Vec<Vec<String>> = cfg
        .keys
        .iter()
        .map(|k| {
            k.trim_start_matches('.')
                .split('.')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .filter(|p: &Vec<String>| !p.is_empty())
        .collect();
    // No key given: sort by the whole value (an empty path resolves to the
    // record itself). Without this the comparator sees no paths and reports
    // every record equal, which silently collapses all rows to one under `-u`.
    if paths.is_empty() {
        vec![Vec::new()]
    } else {
        paths
    }
}

fn run_json(cfg: &Config, start: Instant, lines_mode: bool) -> io::Result<Outcome> {
    let data = read_all(&cfg.files, b'\n')?;
    let paths = parse_paths(cfg);

    let mut values: Vec<serde_json::Value> = if lines_mode {
        let mut v = Vec::new();
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            v.push(serde_json::from_slice(line).map_err(json_err)?);
        }
        v
    } else {
        match serde_json::from_slice(&data).map_err(json_err)? {
            serde_json::Value::Array(a) => a,
            _ => return Err(invalid("--json input must be a JSON array".into())),
        }
    };
    let lines_in = values.len();

    let cmp = |a: &serde_json::Value, b: &serde_json::Value| {
        for p in &paths {
            let o = json_key(a, p).cmp(&json_key(b, p));
            let o = if cfg.reverse { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    };
    values.sort_by(cmp);

    if cfg.unique {
        values.dedup_by(|a, b| {
            paths
                .iter()
                .all(|p| json_key(a, p).cmp(&json_key(b, p)) == Ordering::Equal)
        });
    }
    if let Some(n) = cfg.top {
        values.truncate(n);
    }

    let sink: Box<dyn Write> = match &cfg.output {
        Some(p) => crate::compress::create_output(p)?,
        None => Box::new(io::stdout().lock()),
    };
    let mut w = BufWriter::new(sink);
    if lines_mode {
        for v in &values {
            serde_json::to_writer(&mut w, v).map_err(json_err)?;
            w.write_all(b"\n")?;
        }
    } else {
        serde_json::to_writer_pretty(&mut w, &values).map_err(json_err)?;
        w.write_all(b"\n")?;
    }
    w.flush()?;

    let lines_out = values.len();
    let stats = cfg.stats.then(|| Stats {
        lines_in,
        lines_out,
        duplicates_removed: lines_in.saturating_sub(lines_out),
        chunks: None,
        elapsed_secs: start.elapsed().as_secs_f64(),
    });
    Ok(Outcome {
        exit_code: 0,
        stats,
    })
}

fn json_err(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn jkey_type_ordering() {
        // Null < Bool < Number < String
        assert_eq!(JKey::Null.cmp(&JKey::Bool(false)), Less);
        assert_eq!(JKey::Bool(true).cmp(&JKey::Num(0.0)), Less);
        assert_eq!(JKey::Num(5.0).cmp(&JKey::Str("a".into())), Less);
        assert_eq!(JKey::Num(2.0).cmp(&JKey::Num(10.0)), Less);
        assert_eq!(JKey::Str("a".into()).cmp(&JKey::Str("b".into())), Less);
        assert_eq!(JKey::Bool(false).cmp(&JKey::Bool(true)), Less);
    }

    #[test]
    fn json_key_path_navigation() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"user":{"age":30,"name":"x"}}"#).unwrap();
        assert_eq!(
            json_key(&v, &["user".into(), "age".into()]).cmp(&JKey::Num(30.0)),
            Equal
        );
        // missing path yields Null
        assert_eq!(json_key(&v, &["nope".into()]).cmp(&JKey::Null), Equal);
    }
}
