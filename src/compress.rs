//! Transparent compression for input and output.
//!
//! Input is auto-detected by magic bytes, so it works for stdin and even
//! mis-named files; output is selected by the `-o` file extension. Only gzip
//! and zstd are supported. stdout is never compressed — it has no name to key
//! a format off, matching how GNU `sort` leaves the shell to handle piping.

use std::io::{self, Cursor, Read, Write};
use std::path::Path;

/// A supported compression format (or none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// No compression (raw bytes).
    None,
    /// gzip (RFC 1952).
    Gzip,
    /// Zstandard.
    Zstd,
}

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Detect a compression format from a leading byte prefix (its magic number).
pub fn detect_magic(prefix: &[u8]) -> Compression {
    if prefix.len() >= 2 && prefix[..2] == GZIP_MAGIC {
        Compression::Gzip
    } else if prefix.len() >= 4 && prefix[..4] == ZSTD_MAGIC {
        Compression::Zstd
    } else {
        Compression::None
    }
}

/// Detect a compression format from a file path's extension.
pub fn detect_ext(path: &Path) -> Compression {
    match path.extension().and_then(|e| e.to_str()) {
        Some("gz" | "tgz") => Compression::Gzip,
        Some("zst" | "zstd") => Compression::Zstd,
        _ => Compression::None,
    }
}

/// Read up to `buf.len()` bytes, looping over short reads until full or EOF.
/// Returns the number of bytes actually read.
fn fill(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Wrap a reader so that gzip/zstd input is transparently decompressed.
///
/// Peeks the first four bytes to identify the format, then chains them back in
/// front of the remaining stream so the reader is never consumed when the input
/// is uncompressed. This makes detection work for stdin and mis-named files
/// alike (magic bytes, not the file name, decide the format).
pub fn maybe_decompress(mut r: Box<dyn Read>) -> io::Result<Box<dyn Read>> {
    let mut prefix = [0u8; 4];
    let n = fill(r.as_mut(), &mut prefix)?;
    let kind = detect_magic(&prefix[..n]);
    let chained = Cursor::new(prefix[..n].to_vec()).chain(r);
    Ok(match kind {
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(chained)),
        Compression::Zstd => Box::new(zstd::stream::read::Decoder::new(chained)?),
        Compression::None => Box::new(chained),
    })
}

/// Wrap a writer to compress output in the given format.
///
/// [`Compression::None`] returns the writer unchanged. The compressing wrappers
/// finish their stream (writing the gzip trailer / final zstd frame) when
/// dropped, so the caller must drop the returned writer after its final
/// `flush()` for the output to be complete.
pub fn wrap_writer(w: Box<dyn Write>, kind: Compression) -> io::Result<Box<dyn Write>> {
    Ok(match kind {
        Compression::None => w,
        Compression::Gzip => Box::new(flate2::write::GzEncoder::new(
            w,
            flate2::Compression::default(),
        )),
        Compression::Zstd => Box::new(zstd::stream::write::Encoder::new(w, 0)?.auto_finish()),
    })
}

/// Create `path` for writing, compressing by its extension. Both the
/// `File::create` and the compressor-setup errors are annotated with the path,
/// so a failure names the file the way `File::open`/`File::create` already do.
pub fn create_output(path: &Path) -> io::Result<Box<dyn Write>> {
    let ctx = |e: io::Error| io::Error::new(e.kind(), format!("{}: {}", path.display(), e));
    let f = std::fs::File::create(path).map_err(ctx)?;
    wrap_writer(Box::new(f), detect_ext(path)).map_err(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_detection() {
        assert_eq!(detect_magic(&[0x1f, 0x8b, 0x08]), Compression::Gzip);
        assert_eq!(detect_magic(&[0x28, 0xb5, 0x2f, 0xfd]), Compression::Zstd);
        assert_eq!(detect_magic(b"hello"), Compression::None);
        assert_eq!(detect_magic(&[0x1f]), Compression::None); // too short
        assert_eq!(detect_magic(b""), Compression::None);
    }

    #[test]
    fn ext_detection() {
        assert_eq!(detect_ext(Path::new("a.gz")), Compression::Gzip);
        assert_eq!(detect_ext(Path::new("a.tgz")), Compression::Gzip);
        assert_eq!(detect_ext(Path::new("a.zst")), Compression::Zstd);
        assert_eq!(detect_ext(Path::new("a.zstd")), Compression::Zstd);
        assert_eq!(detect_ext(Path::new("a.txt")), Compression::None);
        assert_eq!(detect_ext(Path::new("a")), Compression::None);
    }

    fn round_trip(kind: Compression) {
        let payload = b"banana\napple\ncherry\n".repeat(100);
        let tf = tempfile::NamedTempFile::new().unwrap();
        {
            let f = std::fs::File::create(tf.path()).unwrap();
            let mut w = wrap_writer(Box::new(f), kind).unwrap();
            w.write_all(&payload).unwrap();
            w.flush().unwrap();
        } // drop finishes the compression stream (gzip trailer / final frame)
        let compressed = std::fs::read(tf.path()).unwrap();
        if kind != Compression::None {
            assert_ne!(compressed, payload, "expected compressed bytes to differ");
        }
        let mut out = Vec::new();
        maybe_decompress(Box::new(Cursor::new(compressed)))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn gzip_round_trip() {
        round_trip(Compression::Gzip);
    }

    #[test]
    fn zstd_round_trip() {
        round_trip(Compression::Zstd);
    }

    #[test]
    fn plain_passthrough() {
        round_trip(Compression::None);
    }

    fn create_output_round_trip(ext: &str) {
        let payload = b"one\ntwo\nthree\n";
        let tf = tempfile::Builder::new().suffix(ext).tempfile().unwrap();
        {
            let mut w = create_output(tf.path()).unwrap();
            w.write_all(payload).unwrap();
            w.flush().unwrap();
        } // drop finishes the compression stream
        let compressed = std::fs::read(tf.path()).unwrap();
        assert_ne!(compressed, payload, "create_output should compress .{ext}");
        let mut out = Vec::new();
        maybe_decompress(Box::new(std::fs::File::open(tf.path()).unwrap()))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn create_output_compresses_by_extension() {
        create_output_round_trip(".gz");
        create_output_round_trip(".zst");
    }
}
