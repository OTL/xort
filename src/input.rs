//! Input collection and line splitting.
//!
//! Milestone 1 reads all inputs into one owned buffer and produces zero-copy
//! line slices that borrow it. (mmap and the streaming/external paths arrive in
//! later milestones.)

use indicatif::ProgressBar;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Read every input (files, or stdin when the list is empty or contains `-`)
/// into a single buffer. A terminator is inserted between inputs that do not
/// already end with one, so line splitting never merges the last line of one
/// file with the first line of the next.
pub fn read_all(files: &[PathBuf], terminator: u8) -> io::Result<Vec<u8>> {
    read_all_with(files, terminator, None)
}

/// Like [`read_all`], but threads an optional progress bar through the reads so
/// the `--progress` byte counter advances as input is consumed. Progress is
/// counted on the raw (pre-decompression) byte stream so it lines up with the
/// on-disk file sizes used for the bar length.
pub fn read_all_with(
    files: &[PathBuf],
    terminator: u8,
    pb: Option<&ProgressBar>,
) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    if files.is_empty() {
        read_stdin(&mut buf, pb)?;
    } else {
        for path in files {
            let before = buf.len();
            if path.as_os_str() == "-" {
                read_stdin(&mut buf, pb)?;
            } else {
                read_file(path, &mut buf, pb)?;
            }
            // Separate inputs that lack a trailing terminator.
            if buf.len() > before && *buf.last().unwrap() != terminator {
                buf.push(terminator);
            }
        }
    }
    Ok(buf)
}

/// Read each input into its own buffer (used by `-m` merge so files stay
/// separate). Empty file list reads stdin as a single buffer.
pub fn read_each(files: &[PathBuf], terminator: u8) -> io::Result<Vec<Vec<u8>>> {
    read_each_with(files, terminator, None)
}

/// Like [`read_each`], but threads an optional progress bar through the reads
/// so `--progress` advances during a `-m` merge as well.
pub fn read_each_with(
    files: &[PathBuf],
    terminator: u8,
    pb: Option<&ProgressBar>,
) -> io::Result<Vec<Vec<u8>>> {
    if files.is_empty() {
        let mut buf = Vec::new();
        read_stdin(&mut buf, pb)?;
        ensure_terminated(&mut buf, terminator);
        return Ok(vec![buf]);
    }
    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let mut buf = Vec::new();
        if path.as_os_str() == "-" {
            read_stdin(&mut buf, pb)?;
        } else {
            read_file(path, &mut buf, pb)?;
        }
        ensure_terminated(&mut buf, terminator);
        out.push(buf);
    }
    Ok(out)
}

#[inline]
fn ensure_terminated(buf: &mut Vec<u8>, terminator: u8) {
    if !buf.is_empty() && *buf.last().unwrap() != terminator {
        buf.push(terminator);
    }
}

fn read_file(path: &Path, buf: &mut Vec<u8>, pb: Option<&ProgressBar>) -> io::Result<()> {
    // Annotate every failure (open, decoder setup, and decode-time read errors)
    // with the path, so one bad file among many is identifiable.
    let ctx = |e: io::Error| io::Error::new(e.kind(), format!("{}: {}", path.display(), e));
    let f = File::open(path).map_err(ctx)?;
    // Count raw bytes (pre-decompression) so progress matches the file size,
    // then transparently decompress gzip/zstd, detected by magic bytes.
    let raw = wrap_progress(Box::new(f), pb);
    let mut r = crate::compress::maybe_decompress(raw).map_err(ctx)?;
    r.read_to_end(buf).map_err(ctx)?;
    Ok(())
}

/// Read all of stdin into `buf`, transparently decompressing gzip/zstd.
fn read_stdin(buf: &mut Vec<u8>, pb: Option<&ProgressBar>) -> io::Result<()> {
    let raw = wrap_progress(Box::new(io::stdin().lock()), pb);
    let mut r = crate::compress::maybe_decompress(raw)?;
    r.read_to_end(buf)?;
    Ok(())
}

/// Wrap a reader so a present progress bar advances as bytes are read.
fn wrap_progress(r: Box<dyn Read>, pb: Option<&ProgressBar>) -> Box<dyn Read> {
    match pb {
        Some(pb) => Box::new(pb.wrap_read(r)),
        None => r,
    }
}

/// Split `data` into line slices (excluding the terminator). A trailing line
/// without a terminator is still returned.
pub fn split_lines(data: &[u8], terminator: u8) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for pos in memchr::memchr_iter(terminator, data) {
        lines.push(&data[start..pos]);
        start = pos + 1;
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_basic() {
        assert_eq!(
            split_lines(b"a\nb\nc\n", b'\n'),
            vec![&b"a"[..], b"b", b"c"]
        );
    }

    #[test]
    fn split_no_trailing_newline() {
        assert_eq!(split_lines(b"a\nb", b'\n'), vec![&b"a"[..], b"b"]);
    }

    #[test]
    fn split_empty() {
        assert!(split_lines(b"", b'\n').is_empty());
    }

    #[test]
    fn split_blank_lines_preserved() {
        assert_eq!(split_lines(b"a\n\nb\n", b'\n'), vec![&b"a"[..], b"", b"b"]);
    }

    #[test]
    fn read_all_with_progress_bar_reads_file() {
        // A hidden bar exercises the `wrap_progress` Some-arm without drawing.
        let p = std::env::temp_dir().join(format!("xort_rap_{}", std::process::id()));
        std::fs::write(&p, b"x\ny\n").unwrap();
        let pb = ProgressBar::new(4);
        pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        let got = read_all_with(std::slice::from_ref(&p), b'\n', Some(&pb)).unwrap();
        assert_eq!(got, b"x\ny\n");
        // read_each_with takes the same progress path.
        let each = read_each_with(std::slice::from_ref(&p), b'\n', Some(&pb)).unwrap();
        assert_eq!(each, vec![b"x\ny\n".to_vec()]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn corrupt_gzip_error_names_the_file() {
        let p = std::env::temp_dir().join(format!("xort_bad_{}.gz", std::process::id()));
        std::fs::write(&p, b"\x1f\x8b\x08not-a-real-gzip-body").unwrap();
        let err = read_all_with(std::slice::from_ref(&p), b'\n', None).unwrap_err();
        assert!(
            err.to_string().contains(p.to_str().unwrap()),
            "decompression error should name the file: {err}"
        );
        let _ = std::fs::remove_file(&p);
    }
}
