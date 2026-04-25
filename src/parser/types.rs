use arrayvec::ArrayVec;
use smallvec::SmallVec;

// ── Severity level ────────────────────────────────────────────────────────────

/// Log severity level as extracted from the input line (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    /// Attempt to parse a severity keyword from a byte slice (case-insensitive).
    pub fn from_bytes(s: &[u8]) -> Option<Self> {
        match s {
            b"TRACE" | b"trace" | b"TRC" => Some(Self::Trace),
            b"DEBUG" | b"debug" | b"DBG" => Some(Self::Debug),
            b"INFO" | b"info" | b"INF" => Some(Self::Info),
            b"WARN" | b"warn" | b"WRN" | b"WARNING" | b"warning" => Some(Self::Warn),
            b"ERROR" | b"error" | b"ERR" => Some(Self::Error),
            b"FATAL" | b"fatal" | b"CRIT" | b"crit" | b"CRITICAL" => Some(Self::Fatal),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
        }
    }
}

// ── Field (key=value pair) ────────────────────────────────────────────────────

/// A single key=value pair parsed from a structured log line (§5.2).
/// Borrows from the input buffer — zero allocation.
#[derive(Debug, Clone, Copy)]
pub struct Field<'buf> {
    pub key: &'buf [u8],
    pub value: &'buf [u8],
}

/// Owned version of `Field` for cross-thread transport.
#[derive(Debug, Clone)]
pub struct OwnedField {
    pub key: Box<[u8]>,
    pub value: Box<[u8]>,
}

impl<'buf> Field<'buf> {
    /// Convert to an owned `OwnedField`, copying both byte slices onto the heap.
    /// Named `into_owned` rather than `to_owned` to avoid shadowing the
    /// blanket `ToOwned` impl from `std` (since `Field` is `Clone + Copy`).
    pub fn into_owned(self) -> OwnedField {
        OwnedField {
            key: self.key.into(),
            value: self.value.into(),
        }
    }
}

// ── Record source ─────────────────────────────────────────────────────────────

/// Tracks where a `LogRecord` originated so the clusterer can shard by path
/// and the formatter can group output by field origin (§5.2, §18.11).
#[derive(Debug, Clone, Copy)]
pub enum RecordSource<'buf> {
    /// Came directly from a line in the input (log / NDJSON mode).
    LogLine,
    /// Extracted from a JSON string value at the given dot-bracket path (§6.3).
    JsonField { path: &'buf str },
}

/// Owned version of `RecordSource` for cross-thread transport.
#[derive(Debug, Clone)]
pub enum OwnedRecordSource {
    LogLine,
    JsonField { path: String },
}

impl<'buf> RecordSource<'buf> {
    /// Convert to `OwnedRecordSource` for cross-thread transport.
    /// Named `into_owned` to avoid shadowing the blanket `ToOwned` impl
    /// (since `RecordSource` is `Clone + Copy`).
    pub fn into_owned(self) -> OwnedRecordSource {
        match self {
            Self::LogLine => OwnedRecordSource::LogLine,
            Self::JsonField { path } => OwnedRecordSource::JsonField {
                path: path.to_owned(),
            },
        }
    }

    /// Returns the JSON path string, or `""` for log-line records.
    pub fn path_str(self) -> &'buf str {
        match self {
            Self::LogLine => "",
            Self::JsonField { path } => path,
        }
    }
}

impl OwnedRecordSource {
    pub fn path_str(&self) -> &str {
        match self {
            Self::LogLine => "",
            Self::JsonField { path } => path.as_str(),
        }
    }
}

// ── LogRecord (borrowed) ──────────────────────────────────────────────────────

/// A single parsed log event, borrowing all byte slices from the input buffer.
///
/// Lifetime `'buf` is tied to the chunk buffer — records must be clustered
/// (or converted to `OwnedLogRecord`) before the buffer is released (§5.2).
#[derive(Debug)]
pub struct LogRecord<'buf> {
    /// Unix microseconds, if a timestamp was successfully parsed.
    pub timestamp: Option<i64>,
    /// Severity level, if detected.
    pub level: Option<Level>,
    /// The primary message text (after stripping timestamp / level prefix).
    pub message: &'buf [u8],
    /// Up to 8 key=value fields on the stack; spills to heap for richer records.
    pub fields: SmallVec<[Field<'buf>; 8]>,
    /// The raw input line (including any prefix stripped from `message`).
    pub raw_line: &'buf [u8],
    /// Byte offset within the original file/stream.
    pub byte_offset: u64,
    /// Origin — distinguishes log lines from JSON-extracted text blobs (§6.3).
    pub source: RecordSource<'buf>,
}

impl<'buf> LogRecord<'buf> {
    /// Convert to an owned `OwnedLogRecord` suitable for sending across threads.
    pub fn to_owned(&self) -> OwnedLogRecord {
        OwnedLogRecord {
            timestamp: self.timestamp,
            level: self.level,
            message: self.message.into(),
            fields: self.fields.iter().map(|f| f.into_owned()).collect(),
            raw_line: self.raw_line.into(),
            byte_offset: self.byte_offset,
            source: self.source.into_owned(),
        }
    }
}

// ── OwnedLogRecord ────────────────────────────────────────────────────────────

/// Heap-allocated mirror of `LogRecord` that can be sent across thread boundaries.
/// This is the type that flows through crossbeam channels into the `ShardedDrain`.
#[derive(Debug, Clone)]
pub struct OwnedLogRecord {
    pub timestamp: Option<i64>,
    pub level: Option<Level>,
    pub message: Box<[u8]>,
    pub fields: Vec<OwnedField>,
    pub raw_line: Box<[u8]>,
    pub byte_offset: u64,
    pub source: OwnedRecordSource,
}

impl OwnedLogRecord {
    /// Convenience: returns the source path for shard-key computation (§7.2).
    pub fn source_path(&self) -> &str {
        self.source.path_str()
    }

    /// Returns the message as a `&str` if valid UTF-8, otherwise empty string.
    pub fn message_str(&self) -> &str {
        std::str::from_utf8(&self.message).unwrap_or("")
    }

    /// Count whitespace-delimited words in the message (used by Drain for shard key).
    pub fn word_count(&self) -> usize {
        self.message_str().split_ascii_whitespace().count()
    }
}

// ── Run statistics ────────────────────────────────────────────────────────────

/// Counters collected during a single lumen run, printed in `--verbose` mode.
#[derive(Debug, Default, Clone)]
pub struct RunStats {
    pub total_bytes: u64,
    pub total_lines: u64,
    pub unparseable_lines: u64,
    pub text_fields_extracted: u64,
    pub elapsed_ms: u64,
}
impl std::fmt::Display for RunStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "total_bytes: {}, total_lines: {}, unparseable_lines: {}, text_fields_extracted: {}, elapsed_ms: {}", self.total_bytes, self.total_lines, self.unparseable_lines, self.text_fields_extracted, self.elapsed_ms)
    }
}
