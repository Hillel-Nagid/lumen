pub mod drain;
pub mod template;

use std::sync::Mutex;

use ahash::AHashMap;

use crate::cli::Args;
use crate::parser::types::OwnedLogRecord;
use crate::scorer::cms::CountMinSketch;

use drain::DrainShard;
use template::{compute_template_id, LogTemplate, TemplateId, Token};

// ── Shard key ─────────────────────────────────────────────────────────────────

/// Compute the shard index for a record using its source path, level, and
/// message word count (§7.2).
///
/// Records from the same JSON path always map to the same shard, ensuring
/// that templates are naturally grouped by field origin (§18.11).
fn shard_key(record: &OwnedLogRecord, shard_count: usize) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = ahash::AHasher::default();
    record.source_path().hash(&mut hasher);
    if let Some(lvl) = record.level {
        (lvl as u8).hash(&mut hasher);
    }
    (record.word_count() as u64).hash(&mut hasher);
    (hasher.finish() as usize) % shard_count
}

// ── Sharded Drain ─────────────────────────────────────────────────────────────

/// Parallel Drain clusterer: `num_cpus × 2` shards, each protected by a `Mutex`
/// and driven concurrently by a `rayon` thread pool (§7.2).
///
/// Memory model: shard `Mutex`es are only contended when two rayon workers hash
/// to the same shard. With `num_cpus × 2` shards the expected contention is ≤ 5%
/// of wall time (§7.2 target).
pub struct ShardedDrain {
    shards:      Vec<Mutex<DrainShard>>,
    shard_count: usize,
}

impl ShardedDrain {
    /// Create a new `ShardedDrain` from CLI arguments.
    ///
    /// Shard count = `min(num_cpus × 2, 128)` unless `--threads` overrides the
    /// CPU count (§7.2).
    pub fn from_args(args: &Args) -> Self {
        let cpus = args.threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });
        let shard_count = (cpus * 2).min(128);
        let shards = (0..shard_count)
            .map(|_| {
                Mutex::new(DrainShard::new(
                    args.depth,
                    args.max_children,
                    args.sim_threshold,
                ))
            })
            .collect();
        Self { shards, shard_count }
    }

    /// Insert a record into the appropriate shard.
    pub fn insert(&self, record: &OwnedLogRecord) {
        let idx = shard_key(record, self.shard_count);
        // SAFETY: idx is always < shard_count == shards.len()
        let mut shard = self.shards[idx].lock().expect("drain shard poisoned");
        shard.insert(record);
    }

    /// Consume all shards, run the merge pass (§18.2), and return the final
    /// deduplicated template list.
    ///
    /// This is a single-threaded post-processing step (§18.2: "merge runs once,
    /// after all shards finalised, on a single thread").
    pub fn finalise(self) -> Vec<LogTemplate> {
        // Collect templates from all shards.
        let mut all_templates: Vec<LogTemplate> = self
            .shards
            .into_iter()
            .flat_map(|m| {
                m.into_inner()
                    .expect("drain shard poisoned at finalise")
                    .take_templates()
            })
            .collect();

        merge_pass(&mut all_templates);
        all_templates
    }
}

// ── Merge pass (§18.2) ────────────────────────────────────────────────────────

/// Unify templates from different shards that differ by exactly one token (§18.2).
///
/// Algorithm:
/// 1. Group templates by token count.
/// 2. Within each group, compare all pairs A and B:
///    - If `same_length(A, B) && token_edit_distance(A, B) == 1`:
///      - Create C with the differing position wildcarded.
///      - `C.count = A.count + B.count`
///      - `CMS(C) = CMS(A) + CMS(B)` (handled at score time; C carries combined count)
///      - Remove A and B; insert C.
/// 3. Repeat until no more merges are possible (convergence).
///
/// TODO(§18.2): Implement the O(templates²) merge pass.
/// In practice it is O(templates) due to shard locality — most templates
/// from different shards already differ at more than one position.
fn merge_pass(templates: &mut Vec<LogTemplate>) {
    let _ = templates;
    // TODO(§18.2): implement token-edit-distance merge
}

// ── Run-count extraction ──────────────────────────────────────────────────────

/// Extract `(TemplateId, count)` pairs from a template list for CMS flushing.
pub fn extract_run_counts(templates: &[LogTemplate]) -> Vec<(TemplateId, u64)> {
    templates
        .iter()
        .map(|t| (t.id, t.occurrence_count()))
        .collect()
}
