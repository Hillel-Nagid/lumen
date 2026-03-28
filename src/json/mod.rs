pub mod classify;
pub mod schema;

use std::sync::Arc;

use crate::cli::Args;
use crate::parser::types::OwnedLogRecord;

pub use classify::TextClass;
pub use schema::{SchemaNode, SchemaTree};

// ── JSON document summary ─────────────────────────────────────────────────────

/// The structural view of a JSON document produced by `JsonDocAnalyzer` (§6).
///
/// Passed to the `Formatter` as a header that precedes the cluster / scored output.
#[derive(Debug)]
pub struct JsonDocSummary {
    /// Inferred schema trie (§6.4).
    pub schema: SchemaTree,
    /// Source filename or `"<stdin>"`.
    pub source_name: String,
    /// Total bytes of the input document.
    pub total_bytes: u64,
    /// Maximum schema tree depth observed.
    pub schema_depth: usize,
    /// Number of string values extracted as `LogRecord`s (§6.3).
    pub text_fields_extracted: u64,
    /// Wall-clock milliseconds elapsed during analysis.
    pub elapsed_ms: u64,
}

// ── JSON document analyzer ────────────────────────────────────────────────────

/// Streams through a single JSON document, simultaneously:
///
/// 1. Building a `SchemaTree` for the structural summary output (§6.4).
/// 2. Classifying every string value as `Scalar`, `UnstructuredLine`, or
///    `UnstructuredMultiline` (§6.3).
/// 3. Emitting `OwnedLogRecord`s for unstructured text fields into the shared
///    pipeline channel — these records flow through Drain → Scorer → Compressor
///    exactly as if they came from a log file (§6, architecture diagram).
///
/// Uses `sonic-rs` in tape (event) mode so the document is never fully
/// materialised in memory — memory ceiling ≤ 64 MB RSS (§6.2).
pub struct JsonDocAnalyzer {
    entropy_threshold: f64,
    max_depth:         usize,
    max_array_samples: usize,
    max_array_inline:  usize,
    schema_only:       bool,
    json_path_filter:  Option<String>,
}

impl JsonDocAnalyzer {
    /// Create a new analyzer from the CLI arguments.
    pub fn from_args(args: &Args) -> Self {
        Self {
            entropy_threshold: args.entropy_threshold,
            max_depth:         args.max_depth,
            max_array_samples: args.max_array_samples,
            max_array_inline:  args.max_array_inline,
            schema_only:       args.schema_only,
            json_path_filter:  args.json_path.clone(),
        }
    }

    /// Analyse the document contained in `data`, stream `OwnedLogRecord`s through
    /// `record_tx`, and return the structural `JsonDocSummary`.
    ///
    /// TODO(§6.2): Implement sonic-rs tape-mode streaming:
    /// - Parse in 32 MB overlapping windows with tape stitching.
    /// - Walk tape tokens (ObjectStart, Key, Str, U64, F64, ArrayStart, ArrayEnd, …).
    /// - For each Str token: run `classify::classify_string()`, then either
    ///   record cardinality stats or emit an OwnedLogRecord.
    /// - Apply array reservoir sampling (§18.6: reservoir size 200).
    /// - Enforce `--max-depth` truncation (§6.6).
    /// - Trigger schema trie scavenger at 90 MB (§18.9).
    pub fn analyze(
        &self,
        data: &[u8],
        source_name: &str,
        record_tx: &crossbeam_channel::Sender<OwnedLogRecord>,
    ) -> crate::error::Result<JsonDocSummary> {
        let _ = (data, record_tx); // suppress unused warnings during boilerplate phase
        todo!("§6.2: sonic-rs tape-mode JSON document analysis")
    }

    /// Second-pass leaf scan for `--json-path` expressions containing `[*]` (§18.8).
    ///
    /// Streams the document a second time, collecting all values at the matched
    /// leaf path without sampling.
    pub fn leaf_scan(
        &self,
        data: &[u8],
        path_expr: &str,
    ) -> crate::error::Result<Vec<LeafValue>> {
        let _ = (data, path_expr);
        todo!("§18.8: --json-path [*] wildcard leaf-scan second pass")
    }
}

// ── Supporting types ──────────────────────────────────────────────────────────

/// A single value collected during a leaf-scan pass (§18.8).
#[derive(Debug, Clone)]
pub struct LeafValue {
    /// Dot-bracket path to this value, with concrete array indices.
    pub path:  Arc<str>,
    /// The raw string representation of the value.
    pub value: String,
}

/// The polymorphic-key tier computed over array element samples (§18.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTier {
    /// Present in > 80% of sampled elements — rendered inline.
    Common,
    /// Present in 20–80% of elements — annotated with occupancy percentage.
    Partial,
    /// Present in < 20% of elements — moved to Schema Delta footnote.
    Polymorphic,
}

/// Classify a key's tier based on its presence ratio across sampled elements (§18.7).
pub fn classify_key_tier(present_count: u64, total_sampled: u64) -> KeyTier {
    if total_sampled == 0 {
        return KeyTier::Common;
    }
    let ratio = present_count as f64 / total_sampled as f64;
    if ratio > 0.80 {
        KeyTier::Common
    } else if ratio >= 0.20 {
        KeyTier::Partial
    } else {
        KeyTier::Polymorphic
    }
}
