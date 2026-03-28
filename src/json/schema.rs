use arrayvec::ArrayVec;
use smol_str::SmolStr;
use std::collections::HashMap;
use std::sync::Arc;

// ── JSON type classification ───────────────────────────────────────────────────

/// The seven JSON value types encountered during tape parsing (§6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonType {
    Null,
    Bool,
    Int,
    Float,
    String,
    Object,
    Array,
}

impl JsonType {
    pub const COUNT: usize = 7;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Null   => "null",
            Self::Bool   => "bool",
            Self::Int    => "integer",
            Self::Float  => "float",
            Self::String => "string",
            Self::Object => "object",
            Self::Array  => "array",
        }
    }
}

// ── Type count table ──────────────────────────────────────────────────────────

/// Per-path counts of each observed JSON type (§6.4).
/// Used to detect type-variance errors (§18.7).
#[derive(Debug, Default, Clone)]
pub struct JsonTypeCounts {
    pub null:   u64,
    pub bool_:  u64,
    pub int:    u64,
    pub float:  u64,
    pub string: u64,
    pub object: u64,
    pub array:  u64,
}

impl JsonTypeCounts {
    pub fn increment(&mut self, ty: JsonType) {
        match ty {
            JsonType::Null   => self.null   += 1,
            JsonType::Bool   => self.bool_  += 1,
            JsonType::Int    => self.int    += 1,
            JsonType::Float  => self.float  += 1,
            JsonType::String => self.string += 1,
            JsonType::Object => self.object += 1,
            JsonType::Array  => self.array  += 1,
        }
    }

    pub fn total(&self) -> u64 {
        self.null + self.bool_ + self.int + self.float
            + self.string + self.object + self.array
    }

    /// Returns the dominant type (highest count), or `None` if all counts are zero.
    pub fn dominant(&self) -> Option<JsonType> {
        let counts = [
            (self.null,   JsonType::Null),
            (self.bool_,  JsonType::Bool),
            (self.int,    JsonType::Int),
            (self.float,  JsonType::Float),
            (self.string, JsonType::String),
            (self.object, JsonType::Object),
            (self.array,  JsonType::Array),
        ];
        counts.iter().max_by_key(|(n, _)| *n).filter(|(n, _)| *n > 0).map(|(_, ty)| *ty)
    }
}

// ── Numeric statistics ────────────────────────────────────────────────────────

/// Summary statistics for a numeric schema node (§6.4).
///
/// `p50` and `p99` are approximated via a t-digest sketch; `min` and `max` are exact.
///
/// TODO(§6.4): Replace `p50`/`p99` fields with a live `tdigest::TDigest` that is
/// finalised at render time. Currently stores exact values as a placeholder.
#[derive(Debug, Clone, Default)]
pub struct NumericStats {
    pub min:   f64,
    pub max:   f64,
    pub p50:   f64,
    pub p99:   f64,
    pub count: u64,
    // TODO(§6.4): tdigest::TDigest for streaming quantile estimation
}

impl NumericStats {
    pub fn new(first: f64) -> Self {
        Self { min: first, max: first, p50: first, p99: first, count: 1 }
    }

    pub fn update(&mut self, value: f64) {
        if value < self.min { self.min = value; }
        if value > self.max { self.max = value; }
        self.count += 1;
        // TODO(§6.4): feed value into tdigest and recompute p50/p99
    }
}

// ── Cardinality sketch ────────────────────────────────────────────────────────

/// Approximate cardinality estimator for string values at a schema node (§6.4).
///
/// Backed by HyperLogLog++ (14-bit, ≈0.8% error).
///
/// TODO(§6.4): Replace the `count` placeholder with
/// `hyperloglogplus::HyperLogLogPlus<Vec<u8>, ahash::RandomState>`.
#[derive(Debug, Default, Clone)]
pub struct CardinalitySketch {
    // Placeholder — will be replaced with HyperLogLog++ from hyperloglogplus crate.
    count: u64,
}

impl CardinalitySketch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a string value into the sketch.
    pub fn insert(&mut self, _bytes: &[u8]) {
        // TODO(§6.4): hll.insert(bytes)
        self.count += 1;
    }

    /// Approximate distinct value count.
    pub fn estimate(&self) -> u64 {
        // TODO(§6.4): return hll.count() as u64
        self.count
    }
}

// ── Schema node ───────────────────────────────────────────────────────────────

/// A node in the schema trie, representing one path segment in the JSON document.
///
/// Each node tracks value statistics for the field at its path (§6.4).
/// Memory pressure is managed by the Scavenger Thread (§18.9).
#[derive(Debug)]
pub struct SchemaNode {
    /// Full dot-bracket path string, e.g. `"results[].event.level"`.
    pub path: Arc<str>,
    /// Count of each JSON type observed at this path.
    pub type_counts: JsonTypeCounts,
    /// Approximate distinct value count (HyperLogLog++ placeholder).
    pub cardinality: CardinalitySketch,
    /// Numeric statistics, populated if `type_counts.int > 0 || type_counts.float > 0`.
    pub numeric: Option<NumericStats>,
    /// Up to 5 representative string samples (for low-cardinality fields).
    pub samples: ArrayVec<SmolStr, 5>,
    /// Length statistics for array nodes (§6.5).
    pub array_len: Option<NumericStats>,
    /// Total times this path was visited across all sampled elements.
    pub observation_count: u64,
    /// Number of direct child paths (used by Scavenger eviction policy §18.9).
    pub child_count: u64,
    /// Whether this node has been collapsed to `<VAR_MAP>` / `<OPAQUE_JSON>` (§18.9).
    pub evicted: bool,
}

impl SchemaNode {
    pub fn new(path: Arc<str>) -> Self {
        Self {
            path,
            type_counts:       JsonTypeCounts::default(),
            cardinality:       CardinalitySketch::new(),
            numeric:           None,
            samples:           ArrayVec::new(),
            array_len:         None,
            observation_count: 0,
            child_count:       0,
            evicted:           false,
        }
    }

    /// Record a string value at this node.
    pub fn record_string(&mut self, value: &[u8]) {
        self.type_counts.increment(JsonType::String);
        self.cardinality.insert(value);
        self.observation_count += 1;
        // Store up to 5 samples (low-cardinality rendering).
        if self.samples.len() < 5 {
            if let Ok(s) = std::str::from_utf8(value) {
                self.samples.push(SmolStr::new(s));
            }
        }
    }

    /// Record a numeric value at this node.
    pub fn record_numeric(&mut self, value: f64, ty: JsonType) {
        self.type_counts.increment(ty);
        self.observation_count += 1;
        match &mut self.numeric {
            Some(stats) => stats.update(value),
            None => self.numeric = Some(NumericStats::new(value)),
        }
    }

    /// Record a scalar (null / bool) observation.
    pub fn record_scalar(&mut self, ty: JsonType) {
        self.type_counts.increment(ty);
        self.observation_count += 1;
    }
}

// ── Schema tree ───────────────────────────────────────────────────────────────

/// The full schema trie for a JSON document (§6.4, §18.9).
///
/// Maps dot-bracket path strings to `SchemaNode`s.
/// Memory is bounded at 100 MB; the Scavenger Thread evicts long-tail nodes
/// once 90 MB is reached (§18.9).
#[derive(Debug, Default)]
pub struct SchemaTree {
    pub nodes: HashMap<Arc<str>, SchemaNode>,
    /// Approximate allocated memory in bytes (used to trigger the scavenger).
    pub approx_bytes: usize,
}

impl SchemaTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create the node for `path`.
    pub fn node_mut(&mut self, path: Arc<str>) -> &mut SchemaNode {
        self.nodes
            .entry(Arc::clone(&path))
            .or_insert_with(|| SchemaNode::new(path))
    }

    /// Returns `true` if the trie has grown beyond the 90 MB soft limit (§18.9).
    pub fn needs_eviction(&self) -> bool {
        self.approx_bytes >= 90 * 1024 * 1024
    }

    /// Evict long-tail nodes per §18.9 policy.
    ///
    /// TODO(§18.9): Implement scavenger eviction:
    /// 1. Remove nodes where `observation_count < 5`.
    /// 2. Remove internal nodes where `child_count < 2`.
    /// 3. Replace evicted subtrees with `<VAR_MAP>` or `<OPAQUE_JSON>` sentinels.
    pub fn evict_long_tail(&mut self) {
        todo!("§18.9: Scavenger Thread eviction of long-tail nodes")
    }

    pub fn depth(&self) -> usize {
        self.nodes
            .keys()
            .map(|p| p.chars().filter(|&c| c == '.').count() + 1)
            .max()
            .unwrap_or(0)
    }
}
