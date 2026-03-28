use ahash::AHashMap;
use smol_str::SmolStr;

use crate::parser::types::OwnedLogRecord;

use super::template::{compute_template_id, LogTemplate, TemplateId, Token};

// ── Drain prefix-tree node ────────────────────────────────────────────────────

/// A node in the Drain prefix tree (§7.1).
///
/// Interior nodes map a fixed token to their children.
/// Leaf nodes own a `LogTemplate`.
#[derive(Debug)]
pub(crate) enum DrainNode {
    /// Interior: maps a fixed first-token literal to child subtrees.
    Interior {
        children: AHashMap<SmolStr, Box<DrainNode>>,
        /// Wildcard child, present after wildcarding kicks in (§7.4 `--max-children`).
        wildcard_child: Option<Box<DrainNode>>,
    },
    /// Leaf: a set of `LogTemplate`s with the same first-token and word count.
    Leaf {
        clusters: Vec<LogTemplate>,
    },
}

impl DrainNode {
    pub fn new_interior() -> Self {
        Self::Interior {
            children:       AHashMap::new(),
            wildcard_child: None,
        }
    }

    pub fn new_leaf() -> Self {
        Self::Leaf { clusters: Vec::new() }
    }
}

// ── Drain shard ───────────────────────────────────────────────────────────────

/// One shard of the parallel Drain clusterer (§7.2).
///
/// Each shard owns an independent prefix tree and is protected by a `Mutex`
/// in `ShardedDrain`. Records land on a shard deterministically via
/// `hash(source_path, level, word_count) % shard_count`.
pub struct DrainShard {
    /// Depth of the prefix tree (§7.4 `--depth`).
    depth: usize,
    /// Maximum children per interior node before wildcarding (§7.4 `--max-children`).
    max_children: usize,
    /// Similarity threshold for matching a log to an existing cluster (§7.4).
    sim_threshold: f64,
    /// The root of the prefix tree, keyed by word count.
    /// Using word count as the first discriminator matches the Drain paper.
    root: AHashMap<usize, DrainNode>,
    /// All templates created in this shard, indexed by `TemplateId`.
    /// Populated after the tree traversal returns a cluster.
    pub templates: AHashMap<TemplateId, LogTemplate>,
}

impl DrainShard {
    pub fn new(depth: usize, max_children: usize, sim_threshold: f64) -> Self {
        Self {
            depth,
            max_children,
            sim_threshold,
            root:      AHashMap::new(),
            templates: AHashMap::new(),
        }
    }

    /// Insert a record into this shard's Drain tree.
    ///
    /// TODO(§7.1–7.2): Full Drain algorithm:
    /// 1. Tokenise `record.message` into words.
    /// 2. Look up the word-count bucket in `self.root`.
    /// 3. Walk the prefix tree using the leading tokens (up to `self.depth`).
    /// 4. At the leaf, find the cluster with highest Jaccard similarity ≥ `sim_threshold`.
    /// 5. If found: update the template (wildcard new variable positions, update count).
    /// 6. If not found: create a new template with all tokens as Literals.
    /// 7. If the leaf exceeds `max_children`: merge all into a wildcard cluster.
    pub fn insert(&mut self, record: &OwnedLogRecord) {
        let _ = record;
        todo!("§7.1: Drain prefix-tree insert")
    }

    /// Drain all templates from this shard for the merge pass (§7.2, §18.2).
    pub fn take_templates(&mut self) -> Vec<LogTemplate> {
        self.templates.drain().map(|(_, t)| t).collect()
    }
}

// ── Token extraction ──────────────────────────────────────────────────────────

/// Tokenise a message byte slice into `SmolStr` words, splitting on ASCII whitespace.
///
/// TODO(§5.3): Replace with a SIMD-accelerated tokeniser that classifies tokens
/// as likely-variable (pure digits, GUIDs, IP addresses, hex strings) and
/// immediately wildcards them, reducing tree depth.
pub(crate) fn tokenise(message: &[u8]) -> Vec<SmolStr> {
    message
        .split(|b| b.is_ascii_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| SmolStr::new(std::str::from_utf8(s).unwrap_or("?")))
        .collect()
}

/// Compute Jaccard similarity between a candidate template's tokens and a
/// fresh token sequence.
///
/// Wildcard positions always count as a match (§7.4 `--sim-threshold`).
pub(crate) fn jaccard_similarity(template_tokens: &[Token], line_tokens: &[SmolStr]) -> f64 {
    if template_tokens.len() != line_tokens.len() {
        return 0.0;
    }
    if template_tokens.is_empty() {
        return 1.0;
    }
    let matches = template_tokens
        .iter()
        .zip(line_tokens.iter())
        .filter(|(t, w)| match t {
            Token::Wildcard       => true,
            Token::Literal(lit)   => lit.as_str() == w.as_str(),
        })
        .count();
    matches as f64 / template_tokens.len() as f64
}

/// Build a token sequence from a raw tokenised line, treating `candidate_tokens`
/// as a guide: positions where the candidate already has a wildcard stay
/// wildcarded; positions where the strings differ become wildcards.
pub(crate) fn update_template_tokens(
    existing: &[Token],
    new_words: &[SmolStr],
) -> (Vec<Token>, bool /* changed */) {
    debug_assert_eq!(existing.len(), new_words.len());
    let mut changed = false;
    let updated = existing
        .iter()
        .zip(new_words.iter())
        .map(|(t, w)| match t {
            Token::Wildcard => Token::Wildcard,
            Token::Literal(lit) => {
                if lit.as_str() == w.as_str() {
                    t.clone()
                } else {
                    changed = true;
                    Token::Wildcard
                }
            }
        })
        .collect();
    (updated, changed)
}
