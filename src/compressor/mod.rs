use crate::scorer::{Promotion, ScoredTemplate};

// ── Variable slot statistics ──────────────────────────────────────────────────

/// Summary statistics for one wildcard slot in a template (§9.1).
///
/// Collected by scanning the examples and all matched records during the
/// clustering phase.
#[derive(Debug, Clone)]
pub struct SlotStats {
    /// Whether all observed values for this slot were numeric.
    pub is_numeric: bool,
    /// Numeric stats (populated when `is_numeric` is true).
    pub numeric: Option<NumericSlotStats>,
    /// Low-cardinality string value set (populated when distinct ≤ 20).
    pub string_set: Option<Vec<String>>,
    /// Approximate distinct count when cardinality > 20.
    pub high_cardinality: Option<u64>,
}

/// Numeric distribution for a single wildcard slot (§9.1).
#[derive(Debug, Clone)]
pub struct NumericSlotStats {
    pub min: f64,
    pub max: f64,
    pub p50: f64,
    pub p99: f64,
}

// ── Variable slot ─────────────────────────────────────────────────────────────

/// A single wildcard position with its observed value statistics (§9.1).
#[derive(Debug, Clone)]
pub struct VariableSlot {
    /// Zero-based position of this wildcard in the template token sequence.
    pub position: usize,
    pub stats: SlotStats,
}

// ── Compressor output ─────────────────────────────────────────────────────────

/// A single entry in the compressor's output — a condensed, lossy representation
/// of a scored template group (§9.1, §9.2).
#[derive(Debug)]
pub struct CompressedEntry {
    /// Rendered pattern, e.g. `"Connected to * in *ms"`.
    pub template: String,
    pub count: u64,
    pub promotion: Promotion,
    pub surprise: f64,
    pub historic_est: u32,
    pub slots: Vec<VariableSlot>,
    pub first_seen_us: i64,
    pub last_seen_us: i64,
    /// Source JSON paths (empty for pure log-mode entries). §18.11
    pub source_paths: Vec<String>,
    /// Example raw lines (up to 3).
    pub examples: Vec<String>,
}

// ── Compressor ────────────────────────────────────────────────────────────────

/// Transforms ranked `ScoredTemplate`s into `CompressedEntry`s (§9).
///
/// This is the **lossy** step — original log lines are not recoverable
/// from the output.
pub struct Compressor {
    /// Approximate bytes-per-token for token-budget trimming (§18.4).
    bytes_per_token: f64,
    /// If set, cap total output tokens.
    token_budget: Option<u64>,
}

impl Compressor {
    pub fn new(bytes_per_token: f64, token_budget: Option<u64>) -> Self {
        Self {
            bytes_per_token,
            token_budget,
        }
    }

    /// Compress a ranked list of scored templates into condensed output entries.
    ///
    /// Token budget enforcement (§10.3):
    /// 1. Always emit all Novelty and Anomaly entries.
    /// 2. Fill remaining budget with Normal/Elevated entries, sorted by count desc.
    /// 3. Append a truncation notice if entries are dropped.
    ///
    /// TODO(§9.1): implement slot-statistics extraction from template examples.
    /// TODO(§9.2): implement semantic delta rendering for runs of the same template.
    /// TODO(§9.3): apply Zstd dictionary compression to intermediate buffers.
    pub fn compress(&self, scored: Vec<ScoredTemplate>) -> Vec<CompressedEntry> {
        let entries = scored
            .into_iter()
            .map(|st| {
                let template = st.template.pattern();
                let count = st.template.occurrence_count();
                let source_paths = st
                    .template
                    .source_paths
                    .into_iter()
                    .map(|p| p.to_string())
                    .collect();
                let examples = st
                    .template
                    .examples
                    .iter()
                    .map(|e| String::from_utf8_lossy(&e.raw_line).into_owned())
                    .collect();
                CompressedEntry {
                    template,
                    count,
                    promotion: st.promotion,
                    surprise: st.surprise,
                    historic_est: st.historic_estimate,
                    slots: vec![],
                    first_seen_us: st.template.first_seen,
                    last_seen_us: st.template.last_seen,
                    source_paths,
                    examples,
                }
            })
            .collect();
        entries
    }

    /// Train a Zstd dictionary from a sample of raw log bytes (§9.3).
    ///
    /// TODO(§9.3): call `zstd::dict::from_samples` and persist the dictionary.
    pub fn train_dict(&self, _samples: &[&[u8]]) -> crate::error::Result<Vec<u8>> {
        todo!("§9.3: Zstd dictionary training via zstd::dict::from_samples")
    }

    /// Estimated token count for a byte slice using `bytes_per_token` (§18.4).
    pub fn estimate_tokens(&self, bytes: usize) -> u64 {
        (bytes as f64 / self.bytes_per_token) as u64
    }
}
