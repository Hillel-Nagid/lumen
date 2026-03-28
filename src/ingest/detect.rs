use crate::cli::ModeOverride;

/// The resolved input processing mode (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Line-delimited plain-text log stream.
    Log,
    /// Newline-delimited JSON (≥80% of sampled lines are valid JSON objects).
    NdjsonLog,
    /// Single JSON document — object (`{`) or array (`[`).
    JsonDocument,
}

/// Number of bytes read from the input for mode detection.
pub const PROBE_LEN: usize = 4096;

/// Number of lines sampled when testing for NDJSON (§6.1, rule 3).
const NDJSON_SAMPLE_LINES: usize = 200;
/// Fraction of sampled lines that must be valid JSON objects for NDJSON detection.
const NDJSON_MIN_RATIO: f64 = 0.80;

/// Determine the `InputMode` from the first `PROBE_LEN` bytes of input.
///
/// Rules (§6.1, applied in order):
/// 1. If `override_` is not `Auto`, return the override directly.
/// 2. Skip leading BOM (`\xEF\xBB\xBF`) and ASCII whitespace.
/// 3. First non-whitespace byte is `{` or `[` → `JsonDocument`.
/// 4. ≥ 80% of the first 200 non-empty lines are valid JSON objects → `NdjsonLog`.
/// 5. Otherwise → `Log`.
pub fn probe_mode(buf: &[u8], override_: ModeOverride) -> InputMode {
    match override_ {
        ModeOverride::Log  => return InputMode::Log,
        ModeOverride::Json => return InputMode::JsonDocument,
        ModeOverride::Auto => {}
    }

    let buf = strip_bom(buf);

    // Rule 3: single JSON document
    let first = buf.iter().find(|&&b| !b.is_ascii_whitespace()).copied();
    if matches!(first, Some(b'{') | Some(b'[')) {
        return InputMode::JsonDocument;
    }

    // Rule 4: NDJSON heuristic
    if is_ndjson(buf) {
        return InputMode::NdjsonLog;
    }

    InputMode::Log
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Strip a UTF-8 BOM (`\xEF\xBB\xBF`) from the start of a buffer, if present.
fn strip_bom(buf: &[u8]) -> &[u8] {
    if buf.starts_with(b"\xEF\xBB\xBF") {
        &buf[3..]
    } else {
        buf
    }
}

/// Returns `true` if ≥ 80% of the first `NDJSON_SAMPLE_LINES` non-empty lines
/// are valid JSON objects (start with `{` after trimming whitespace).
///
/// This is an intentionally cheap heuristic — we do not attempt to fully parse
/// each line. The full `simd-json` parse happens in the actual parser (§5.3).
fn is_ndjson(buf: &[u8]) -> bool {
    let mut total: usize = 0;
    let mut json_object_lines: usize = 0;

    for line in buf.split(|&b| b == b'\n') {
        let trimmed = line
            .iter()
            .copied()
            .skip_while(|b| b.is_ascii_whitespace())
            .collect::<Vec<_>>();

        if trimmed.is_empty() {
            continue;
        }

        total += 1;
        if trimmed.first() == Some(&b'{') {
            json_object_lines += 1;
        }

        if total >= NDJSON_SAMPLE_LINES {
            break;
        }
    }

    if total == 0 {
        return false;
    }

    (json_object_lines as f64 / total as f64) >= NDJSON_MIN_RATIO
}
