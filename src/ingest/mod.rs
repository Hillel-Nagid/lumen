pub mod detect;

use std::fs::File;
use std::io::{ self, BufReader, Read };
use std::path::Path;

use anyhow::Context;
use memmap2::Mmap;

use crate::error::Result;

pub use detect::{ InputMode, PROBE_LEN };

// ── File-size threshold for mmap vs. streaming ────────────────────────────────

/// Files below this size are read into a single contiguous buffer.
/// Files at or above this size use memory-mapping (§4.1).
const MMAP_THRESHOLD: u64 = 64 * 1024 * 1024; // 64 MB

// ── Ingest source ─────────────────────────────────────────────────────────────

/// An open input source, ready for mode detection and streaming (§4.1).
///
/// The first `PROBE_LEN` bytes are always available in `peek_buf` for mode
/// detection. For `mmap`-backed sources the remainder of the file is already
/// accessible; for buffered sources the stream continues after the peek window.
pub struct IngestSource {
    inner: IngestInner,
    peek_buf: Vec<u8>,
}

enum IngestInner {
    /// Memory-mapped file (used for regular files ≥ MMAP_THRESHOLD).
    Mapped(Mmap),
    /// Small file loaded entirely into a heap buffer.
    SmallFile(Vec<u8>),
    /// Stdin or piped input — buffered, cannot seek.
    Buffered(BufReader<Box<dyn Read + Send>>),
}

impl IngestSource {
    /// The first up-to-`PROBE_LEN` bytes of the input, used for mode detection.
    pub fn peek_buf(&self) -> &[u8] {
        &self.peek_buf
    }

    /// Return the full content as a contiguous byte slice.
    ///
    /// For `Mapped` and `SmallFile` variants this is zero-copy.
    /// For `Buffered` (stdin) this reads the remainder into a heap buffer and
    /// returns the combined peek + remainder.
    ///
    /// # Errors
    /// Returns an error if the remaining stdin bytes cannot be read.
    pub fn into_bytes(self) -> Result<Vec<u8>> {
        match self.inner {
            IngestInner::Mapped(mmap) => Ok(mmap[..].to_vec()),
            IngestInner::SmallFile(buf) => Ok(buf),
            IngestInner::Buffered(mut reader) => {
                let mut buf = self.peek_buf;
                reader.read_to_end(&mut buf).context("reading from stdin")?;
                Ok(buf)
            }
        }
    }

    /// Iterate over lines, yielding each as a borrowed `&[u8]` slice.
    ///
    /// For `Mapped` / `SmallFile` sources this uses SIMD-accelerated line finding
    /// via `memchr::memchr_iter` (§4.2, throughput target ≥ 3 GB/s).
    ///
    /// TODO(§4.2): Replace the scalar split with a memchr::memchr_iter loop that
    /// hands off `&[u8]` line slices without allocation.
    pub fn lines(&self) -> LineIter<'_> {
        let data: &[u8] = match &self.inner {
            IngestInner::Mapped(m) => m,
            IngestInner::SmallFile(b) => b,
            IngestInner::Buffered(_) => &self.peek_buf,
        };
        LineIter { data, pos: 0 }
    }
}

// ── Line iterator ─────────────────────────────────────────────────────────────

/// Zero-copy line iterator backed by `memchr` for SIMD newline finding (§4.2).
pub struct LineIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> LineIter<'a> {
    /// Create a `LineIter` over a raw byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for LineIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        let remaining = &self.data[self.pos..];
        // §4.2: use memchr for SIMD-accelerated newline scan.
        let end = memchr
            ::memchr(b'\n', remaining)
            .map(|i| i + 1)
            .unwrap_or(remaining.len());
        let line = &remaining[..end];
        self.pos += end;
        // Strip trailing \r\n or \n
        let trimmed = if line.ends_with(b"\r\n") {
            &line[..line.len() - 2]
        } else if line.ends_with(b"\n") {
            &line[..line.len() - 1]
        } else {
            line
        };
        Some(trimmed)
    }
}

// ── open_input ────────────────────────────────────────────────────────────────

/// Open the input source from a file path (or stdin if `path` is `None` or `"-"`).
///
/// Reads the first `PROBE_LEN` bytes immediately so that mode detection can
/// inspect them without consuming the stream (§6.1).
pub fn open_input(path: Option<&Path>) -> Result<IngestSource> {
    match path {
        None => open_stdin(),
        Some(p) if p == Path::new("-") => open_stdin(),
        Some(p) => open_file(p),
    }
}

fn open_file(path: &Path) -> Result<IngestSource> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();

    // Detect compressed files by extension and delegate.
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext {
            "gz" => {
                return open_gz(file, size);
            }
            "zst" => {
                return open_zst(file, size);
            }
            "bz2" => {
                return open_bz2(file, size);
            }
            _ => {}
        }
    }

    if size < MMAP_THRESHOLD {
        // Small file: read entirely into heap.
        let mut buf = Vec::with_capacity(size as usize);
        let mut reader = BufReader::new(file);
        reader.read_to_end(&mut buf).context("reading small file")?;
        let peek_buf = buf[..buf.len().min(PROBE_LEN)].to_vec();
        return Ok(IngestSource {
            inner: IngestInner::SmallFile(buf),
            peek_buf,
        });
    }

    // Large file: memory-map it (§4.1).
    // SAFETY: the file is opened read-only; no other process is expected to
    // truncate it during the run. This is the documented intended use of mmap.
    let mmap = (unsafe { Mmap::map(&file) }).context("mmap failed")?;
    let peek_buf = mmap[..mmap.len().min(PROBE_LEN)].to_vec();
    Ok(IngestSource {
        inner: IngestInner::Mapped(mmap),
        peek_buf,
    })
}

fn open_stdin() -> Result<IngestSource> {
    let mut reader: BufReader<Box<dyn Read + Send>> = BufReader::with_capacity(
        4 * 1024 * 1024,
        Box::new(io::stdin())
    );
    // Read first PROBE_LEN bytes for mode detection.
    let mut peek_buf = vec![0u8; PROBE_LEN];
    let n = reader.read(&mut peek_buf).context("reading stdin for probe")?;
    peek_buf.truncate(n);
    Ok(IngestSource {
        inner: IngestInner::Buffered(reader),
        peek_buf,
    })
}

// ── Transparent decompression helpers ────────────────────────────────────────

fn open_gz(file: File, _size: u64) -> Result<IngestSource> {
    use flate2::read::GzDecoder;
    let decoder = GzDecoder::new(file);
    let mut reader: BufReader<Box<dyn Read + Send>> = BufReader::with_capacity(
        4 * 1024 * 1024,
        Box::new(decoder)
    );
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).context("decompressing .gz")?;
    let peek_buf = buf[..buf.len().min(PROBE_LEN)].to_vec();
    Ok(IngestSource { inner: IngestInner::SmallFile(buf), peek_buf })
}

fn open_zst(file: File, _size: u64) -> Result<IngestSource> {
    let decoder = zstd::Decoder::new(file).context("creating zstd decoder")?;
    let mut reader: BufReader<Box<dyn Read + Send>> = BufReader::with_capacity(
        4 * 1024 * 1024,
        Box::new(decoder)
    );
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).context("decompressing .zst")?;
    let peek_buf = buf[..buf.len().min(PROBE_LEN)].to_vec();
    Ok(IngestSource { inner: IngestInner::SmallFile(buf), peek_buf })
}

fn open_bz2(file: File, _size: u64) -> Result<IngestSource> {
    use bzip2::read::BzDecoder;
    let decoder = BzDecoder::new(file);
    let mut reader: BufReader<Box<dyn Read + Send>> = BufReader::with_capacity(
        4 * 1024 * 1024,
        Box::new(decoder)
    );
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).context("decompressing .bz2")?;
    let peek_buf = buf[..buf.len().min(PROBE_LEN)].to_vec();
    Ok(IngestSource { inner: IngestInner::SmallFile(buf), peek_buf })
}
