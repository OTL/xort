//! Input collection and line splitting.
//!
//! Milestone 1 reads all inputs into one owned buffer and produces zero-copy
//! line slices that borrow it. (mmap and the streaming/external paths arrive in
//! later milestones.)

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Read every input (files, or stdin when the list is empty or contains `-`)
/// into a single buffer. A terminator is inserted between inputs that do not
/// already end with one, so line splitting never merges the last line of one
/// file with the first line of the next.
pub fn read_all(files: &[PathBuf], terminator: u8) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    if files.is_empty() {
        io::stdin().lock().read_to_end(&mut buf)?;
    } else {
        for path in files {
            let before = buf.len();
            if path.as_os_str() == "-" {
                io::stdin().lock().read_to_end(&mut buf)?;
            } else {
                read_file(path, &mut buf)?;
            }
            // Separate inputs that lack a trailing terminator.
            if buf.len() > before && *buf.last().unwrap() != terminator {
                buf.push(terminator);
            }
        }
    }
    Ok(buf)
}

fn read_file(path: &Path, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut f = File::open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path.display(), e)))?;
    f.read_to_end(buf)?;
    Ok(())
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
}
