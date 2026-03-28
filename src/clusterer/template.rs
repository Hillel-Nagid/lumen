use arrayvec::ArrayVec;
use smallvec::SmallVec;
use smol_str::SmolStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::parser::types::OwnedLogRecord;

// ── Template identity ─────────────────────────────────────────────────────────

/// Stable 64-bit hash of a template's token pattern.
/// Computed with ahash over the sequence of `Token` variants so that structurally
/// identical templates from different shards share the same ID (§7.3, §18.2).
pub type TemplateId = u64;

// ── Token ─────────────────────────────────────────────────────────────────────

/// A single positional element in a log template (§7.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Token {
    /// A fixed literal word that appears consistently at this position.
    Literal(SmolStr),
    /// A variable slot — the Drain algorithm replaced a high-cardinality word
    /// (or a merged differing token per §18.2) with a wildcard.
    Wildcard,
}

impl Token {
    /// Returns `true` if this token is a wildcard.
    #[inline]
    pub fn is_wildcard(&self) -> bool {
        matches!(self, Self::Wildcard)
    }

    /// Returns the literal string, or `"*"` for wildcards (used in display).
    pub fn as_str(&self) -> &str {
        match self {
            Self::Literal(s) => s.as_str(),
            Self::Wildcard   => "*",
        }
    }
}

// ── LogTemplate ───────────────────────────────────────────────────────────────

/// A clustered log template produced by the Drain algorithm (§7.3).
///
/// `count` is updated atomically so multiple rayon threads can increment it
/// after the merge pass without a mutex.
///
/// `source_paths` accumulates every distinct JSON path whose extracted text
/// records contributed to this template — see §18.11 (cross-path identity).
pub struct LogTemplate {
    /// Stable hash of the token pattern (used as CMS key).
    pub id: TemplateId,
    /// The token sequence (literals + wildcards).
    pub tokens: Vec<Token>,
    /// Total number of input records matched to this template.
    pub count: AtomicU64,
    /// Unix microseconds of the first record matched.
    pub first_seen: i64,
    /// Unix microseconds of the most recently matched record.
    pub last_seen: i64,
    /// JSON paths that contributed records to this template (§18.11).
    /// Empty for pure log-mode templates.
    pub source_paths: SmallVec<[Arc<str>; 2]>,
    /// Up to 3 representative raw records stored as examples.
    pub examples: ArrayVec<OwnedLogRecord, 3>,
}

impl LogTemplate {
    /// Construct a new template from a freshly matched record.
    pub fn new(id: TemplateId, tokens: Vec<Token>, record: &OwnedLogRecord) -> Self {
        let mut examples = ArrayVec::new();
        examples.push(record.clone());

        let mut source_paths: SmallVec<[Arc<str>; 2]> = SmallVec::new();
        let path = record.source_path();
        if !path.is_empty() {
            source_paths.push(Arc::from(path));
        }

        Self {
            id,
            tokens,
            count: AtomicU64::new(1),
            first_seen: record.timestamp.unwrap_or(0),
            last_seen:  record.timestamp.unwrap_or(0),
            source_paths,
            examples,
        }
    }

    /// Increment the occurrence counter and update `last_seen`.
    pub fn record_match(&self, timestamp: Option<i64>) {
        self.count.fetch_add(1, Ordering::Relaxed);
        // last_seen is updated non-atomically in the single-threaded merge/score pass.
        let _ = timestamp;
    }

    /// Returns the current occurrence count.
    pub fn occurrence_count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Number of wildcard positions (variable slots).
    pub fn wildcard_count(&self) -> usize {
        self.tokens.iter().filter(|t| t.is_wildcard()).count()
    }

    /// Register an additional source path for §18.11 multi-path grouping.
    pub fn add_source_path(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        let arc_path: Arc<str> = Arc::from(path);
        if !self.source_paths.iter().any(|p| p.as_ref() == path) {
            self.source_paths.push(arc_path);
        }
    }

    /// Render the template as a human-readable pattern string (e.g. "Connected to * in *ms").
    pub fn pattern(&self) -> String {
        self.tokens
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl std::fmt::Debug for LogTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogTemplate")
            .field("id",      &self.id)
            .field("pattern", &self.pattern())
            .field("count",   &self.occurrence_count())
            .finish()
    }
}

// ── Template ID computation ───────────────────────────────────────────────────

/// Compute a stable `TemplateId` for a token sequence using ahash (§7.3).
pub fn compute_template_id(tokens: &[Token]) -> TemplateId {
    use std::hash::{Hash, Hasher};
    let mut hasher = ahash::AHasher::default();
    tokens.hash(&mut hasher);
    hasher.finish()
}
