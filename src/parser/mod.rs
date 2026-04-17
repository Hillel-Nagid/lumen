pub mod types;

use types::{Field, Level, LogRecord, OwnedLogRecord, RecordSource};

// ── Format hint ───────────────────────────────────────────────────────────────

/// The detected log-line format, locked in after the first 1,000 lines (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatHint {
    /// JSON objects on each line — parsed with `simd-json` (§5.3).
    JsonLines,
    /// `key=value` pairs, RFC 5424 syslog, or similar structured formats.
    Logfmt,
    /// Apache / Nginx Common Log Format.
    CommonLog,
    /// RFC 5424 syslog.
    Syslog,
    /// No consistent structure; treat the whole line as the message.
    Raw,
}

impl FormatHint {
    /// Detect the format from a sample of lines (§5.1).
    ///
    /// Runs on the first 1,000 non-empty lines and locks in the result.
    /// Any subsequent line that fails to parse under the detected format
    /// falls through to `Raw`.
    ///
    /// TODO(§5.1): implement full heuristic scoring:
    /// - JSON: simd-json to_tape on sample lines
    /// - Logfmt: count `key=value` occurrences  
    /// - CommonLog: match Apache CLF regex approximation
    /// - Syslog: check for RFC 5424 PRI field `<N>`
    pub fn detect(sample_lines: &[&[u8]]) -> Self {
        let _ = sample_lines;
        FormatHint::Raw
    }
}

// ── Multiline folder ──────────────────────────────────────────────────────────

/// Configuration for multiline log folding (§18.1).
#[derive(Debug, Clone)]
pub struct MultilineConfig {
    /// If `Some(c)`, a new record begins whenever a line's first non-whitespace
    /// byte equals `c`. Otherwise, indentation-based folding is used.
    pub line_start: Option<u8>,
}

impl MultilineConfig {
    pub fn from_char(c: Option<char>) -> Self {
        Self {
            line_start: c.and_then(|ch| u8::try_from(ch as u32).ok()),
        }
    }

    /// Returns `true` if `line` should begin a new `LogRecord`.
    /// Implements the indentation-based heuristic (§18.1):
    /// a new record starts when the first non-whitespace column is ≤ the
    /// anchor line's first non-whitespace column.
    pub fn is_new_record(&self, line: &[u8], anchor_indent: usize) -> bool {
        if let Some(start_byte) = self.line_start {
            return line.first().copied() == Some(start_byte);
        }
        // Indentation-based: continuation lines are indented further than the anchor.
        let indent = line.iter().take_while(|b| b.is_ascii_whitespace()).count();
        line.is_empty() || indent <= anchor_indent
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// The tiered log parser (§5.1).
///
/// After the format is detected on the first 1,000 lines, every subsequent
/// line is parsed with the corresponding strategy. Lines that fail to parse
/// fall through to the `Raw` tier and are counted in `unparseable_lines`.
pub struct Parser {
    format: FormatHint,
    multiline: MultilineConfig,
}

impl Parser {
    /// Create a new parser. Format detection is deferred to the first call to
    /// `detect_format`.
    pub fn new(multiline: MultilineConfig) -> Self {
        Self {
            format: FormatHint::Raw, // will be overridden after detection
            multiline,
        }
    }

    /// Lock in the format hint from a sample of lines (§5.1).
    /// Call this once before the main parsing loop.
    pub fn detect_format(&mut self, sample: &[&[u8]]) {
        self.format = FormatHint::detect(sample);
    }

    /// Parse a single raw line and return a borrowed `LogRecord`.
    ///
    /// The returned record borrows `line` — the caller must call
    /// `LogRecord::to_owned()` before the line buffer is invalidated.
    ///
    /// Unparseable lines fall through to `Raw` and are returned with
    /// `message = line` and no extracted fields.
    pub fn parse_line<'buf>(
        &self,
        line: &'buf [u8],
        byte_offset: u64,
    ) -> (LogRecord<'buf>, bool /* was_raw_fallback */) {
        let _ = byte_offset;
        match self.format {
            FormatHint::JsonLines => self.parse_json_line(line, byte_offset),
            FormatHint::Logfmt => self.parse_logfmt(line, byte_offset),
            FormatHint::CommonLog => self.parse_common_log(line, byte_offset),
            FormatHint::Syslog => self.parse_syslog(line, byte_offset),
            FormatHint::Raw => (make_raw_record(line, byte_offset), false),
        }
    }

    // ── Per-format parsers ────────────────────────────────────────────────────

    fn parse_json_line<'buf>(&self, line: &'buf [u8], offset: u64) -> (LogRecord<'buf>, bool) {
        let _ = offset;
        // TODO(§5.3): simd-json zero-copy parse into LogRecord fields
        (make_raw_record(line, offset), true)
    }

    fn parse_logfmt<'buf>(&self, line: &'buf [u8], offset: u64) -> (LogRecord<'buf>, bool) {
        // TODO(§5.3): SIMD-accelerated logfmt scanner for `=` and delimiters
        (make_raw_record(line, offset), true)
    }

    fn parse_common_log<'buf>(&self, line: &'buf [u8], offset: u64) -> (LogRecord<'buf>, bool) {
        // TODO(§5.1): Apache Common Log Format parser
        (make_raw_record(line, offset), true)
    }

    fn parse_syslog<'buf>(&self, line: &'buf [u8], offset: u64) -> (LogRecord<'buf>, bool) {
        // TODO(§5.1): RFC 5424 syslog parser
        (make_raw_record(line, offset), true)
    }
}

// ── Raw record construction ───────────────────────────────────────────────────

/// Build a minimal `LogRecord` that treats the whole line as the message.
/// This is the §5.1 "Raw tier" fallback.
pub fn make_raw_record(line: &[u8], byte_offset: u64) -> LogRecord<'_> {
    LogRecord {
        timestamp: None,
        level: None,
        message: line,
        fields: smallvec::SmallVec::new(),
        raw_line: line,
        byte_offset,
        source: RecordSource::LogLine,
    }
}

// ── Multiline fold ────────────────────────────────────────────────────────────

/// Fold a slice of continuation lines into a single `OwnedLogRecord` (§18.1).
///
/// `anchor` is the first line (already parsed as a `LogRecord`).
/// `continuations` are the subsequent indented / continuation lines.
pub fn fold_multiline(anchor: OwnedLogRecord, _continuations: &[&[u8]]) -> OwnedLogRecord {
    // TODO(§18.1): append continuation lines to anchor.raw_line,
    // preserving the first line as anchor.message.
    anchor
}
