/// Classification of a JSON string value for the text field extraction pipeline (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextClass {
    /// Short string or high-entropy blob — ID, enum value, UUID, base64, JWT.
    /// Used for schema cardinality statistics only; never sent to Drain.
    Scalar,
    /// Long string with natural-language word boundaries.
    /// Treated as a single log message and forwarded to the log Parser → Drain pipeline.
    UnstructuredLine,
    /// String containing embedded `\n` (JSON-escaped newlines).
    /// Unescaped, split, and folded into a single `OwnedLogRecord` per §18.1.
    UnstructuredMultiline,
}

// ── Classification rules (§6.3) ───────────────────────────────────────────────

/// Minimum byte length for a string to be considered as unstructured text.
const MIN_TEXT_LEN: usize = 60;
/// Minimum word count for a string to qualify as `UnstructuredLine`.
const MIN_WORD_COUNT: usize = 5;

/// Classify a raw (JSON-unescaped) byte slice according to §6.3 rules,
/// then apply the entropy filter from §18.10.
///
/// The `entropy_threshold` parameter comes from `--entropy-threshold` (default 3.5).
pub fn classify_string(bytes: &[u8], entropy_threshold: f64) -> TextClass {
    // Rule 1: too short or no space → Scalar
    if bytes.len() < MIN_TEXT_LEN || !bytes.contains(&b' ') {
        return TextClass::Scalar;
    }

    // Rule 2: embedded newline → UnstructuredMultiline (before entropy check,
    // because newlines indicate structured multi-line content regardless of entropy)
    if bytes.contains(&b'\n') {
        return TextClass::UnstructuredMultiline;
    }

    // §18.10 entropy filter — runs before word-count/timestamp checks to
    // short-circuit high-entropy strings (JWTs, base64, UUIDs).
    match entropy_filter(bytes) {
        EntropyResult::Opaque => return TextClass::Scalar,
        EntropyResult::HighEntropy(e) if e > entropy_threshold => return TextClass::Scalar,
        _ => {}
    }

    // Rule 3: starts with a timestamp pattern OR contains a level keyword
    if starts_with_timestamp(bytes) || contains_level_keyword(bytes) {
        return TextClass::UnstructuredLine;
    }

    // Rule 4: long enough and has sufficient word density
    if word_count(bytes) >= MIN_WORD_COUNT {
        return TextClass::UnstructuredLine;
    }

    TextClass::Scalar
}

// ── Entropy filter (§18.10) ───────────────────────────────────────────────────

/// Result of the Shannon entropy calculation.
#[derive(Debug, Clone, Copy)]
pub enum EntropyResult {
    /// Contains non-printable bytes — treat as binary / encoded blob.
    Opaque,
    /// All bytes printable; computed Shannon entropy in bits/byte.
    HighEntropy(f64),
    /// All bytes printable; entropy is below the opaque threshold.
    LowEntropy(f64),
}

/// Compute byte-level Shannon entropy using a stack-allocated 256-bucket histogram
/// and a lookup table for `p·log₂(p)` values (§18.10).
///
/// The SIMD histogram increment is expressed as a sequential loop here;
/// TODO(§18.10): replace the inner loop with SIMD 16- or 32-lane increments
/// using `std::arch` or the `packed_simd` / `wide` crate.
pub fn entropy_filter(bytes: &[u8]) -> EntropyResult {
    if bytes.is_empty() {
        return EntropyResult::LowEntropy(0.0);
    }

    // SIMD byte histogram — 256 buckets, stack-allocated (§18.10).
    let mut hist = [0u32; 256];
    for &b in bytes {
        // Opaque fast-exit: non-printable ASCII (outside 0x20–0x7E plus \t \n \r)
        if b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r' {
            return EntropyResult::Opaque;
        }
        if b > 0x7E {
            return EntropyResult::Opaque;
        }
        // SAFETY: b is a valid u8, cast to usize index is always in-bounds.
        hist[b as usize] += 1;
        // TODO(§18.10): replace with SIMD 32-byte lane increment
    }

    let n = bytes.len() as f64;
    let entropy = hist
        .iter()
        .filter(|&&c| c > 0)
        .fold(0.0_f64, |acc, &c| {
            let p = c as f64 / n;
            // Approximate p·log₂(p) via lookup table.
            // TODO(§18.10): replace with pre-built 256-entry f32 LUT for speed.
            acc - p * p.log2()
        });

    if entropy > 3.5 {
        EntropyResult::HighEntropy(entropy)
    } else {
        EntropyResult::LowEntropy(entropy)
    }
}

// ── Helper predicates ─────────────────────────────────────────────────────────

/// Quick check for a timestamp-like prefix (digits followed by `-`, `/`, `:`, `T`, `.`, `Z`).
fn starts_with_timestamp(bytes: &[u8]) -> bool {
    // Heuristic: at least 4 leading digits followed by a date/time separator.
    if bytes.len() < 5 {
        return false;
    }
    bytes[..4].iter().all(|b| b.is_ascii_digit())
        && matches!(bytes[4], b'-' | b'/' | b'T' | b'.')
}

/// Returns `true` if the slice contains a common log level keyword.
fn contains_level_keyword(bytes: &[u8]) -> bool {
    // Use memchr to scan for the first byte of each keyword.
    // TODO(§5.2): replace with SIMD classification of the whole line.
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };
    for word in s.split_ascii_whitespace() {
        match word {
            "TRACE" | "DEBUG" | "INFO" | "WARN" | "WARNING"
            | "ERROR" | "FATAL" | "CRITICAL"
            | "trace" | "debug" | "info" | "warn" | "warning"
            | "error" | "fatal" | "critical" => return true,
            _ => {}
        }
    }
    false
}

/// Count whitespace-delimited words in a byte slice.
fn word_count(bytes: &[u8]) -> usize {
    bytes
        .split(|b| b.is_ascii_whitespace())
        .filter(|s| !s.is_empty())
        .count()
}
