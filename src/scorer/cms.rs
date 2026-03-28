use crate::clusterer::template::TemplateId;

// ── CMS parameters (§8.1) ────────────────────────────────────────────────────

/// Number of counters per row (2^20 ≈ 1 M).
pub const CMS_WIDTH: usize = 1 << 20;
/// Number of independent hash rows.
pub const CMS_DEPTH: usize = 5;

// ── Count-Min Sketch ──────────────────────────────────────────────────────────

/// A Count-Min Sketch that tracks approximate frequencies of `TemplateId`s
/// across the historic runs of a project (§8.1).
///
/// Memory: `CMS_WIDTH × CMS_DEPTH × 4 bytes` = 20 MB.
///
/// Counters are `u32` and saturate at `u32::MAX` to prevent wrapping.
///
/// The CMS is persisted to `cms.bin` (§14) between runs and updated with
/// time-weighted decay after each successful run (§18.3).
pub struct CountMinSketch {
    /// Flat `[row][col]` array stored in row-major order.
    counters: Vec<u32>,
    /// Total number of events recorded in this sketch (for frequency normalisation).
    pub total_events: u64,
    /// Unix timestamp (seconds) of the last run that flushed into this sketch.
    /// Used to compute elapsed time for time-weighted decay (§18.3).
    pub last_run_ts: i64,
}

impl CountMinSketch {
    /// Allocate a fresh (all-zero) sketch.
    pub fn new() -> Self {
        Self {
            counters:     vec![0u32; CMS_WIDTH * CMS_DEPTH],
            total_events: 0,
            last_run_ts:  0,
        }
    }

    /// Increment all `CMS_DEPTH` counters for `id` by `delta` (saturating).
    pub fn increment(&mut self, id: TemplateId, delta: u32) {
        for row in 0..CMS_DEPTH {
            let col = self.col(id, row);
            let counter = &mut self.counters[row * CMS_WIDTH + col];
            *counter = counter.saturating_add(delta);
        }
        self.total_events = self.total_events.saturating_add(delta as u64);
    }

    /// Return the minimum counter value for `id` across all rows (the CMS estimate).
    pub fn estimate(&self, id: TemplateId) -> u32 {
        (0..CMS_DEPTH)
            .map(|row| {
                let col = self.col(id, row);
                self.counters[row * CMS_WIDTH + col]
            })
            .min()
            .unwrap_or(0)
    }

    /// Apply time-weighted decay to all counters before merging a new run (§18.3).
    ///
    /// Decay formula: `counter *= exp(-λ · Δt_hours)`
    /// where `λ = ln(2) / half_life_hours` and `Δt_hours` is the elapsed time
    /// since `last_run_ts`.
    ///
    /// This implements §18.3 (time-weighted decay) rather than the simpler
    /// per-run fixed-factor approach.
    pub fn apply_decay(&mut self, now_ts: i64, half_life_hours: f64) {
        let delta_hours = ((now_ts - self.last_run_ts).max(0) as f64) / 3600.0;
        let lambda = std::f64::consts::LN_2 / half_life_hours;
        let factor = (-lambda * delta_hours).exp() as f32;

        for counter in &mut self.counters {
            *counter = (*counter as f32 * factor) as u32;
        }
        // Scale total_events by the same factor so frequency ratios remain valid.
        self.total_events = (self.total_events as f64 * factor as f64) as u64;
    }

    /// Merge the per-run template counts into this sketch (§8.1).
    ///
    /// Call `apply_decay` first to down-weight stale counts from previous runs.
    pub fn merge_run(&mut self, run_counts: &[(TemplateId, u64)], now_ts: i64) {
        for &(id, count) in run_counts {
            let delta = count.min(u32::MAX as u64) as u32;
            self.increment(id, delta);
        }
        self.last_run_ts = now_ts;
    }

    // ── Serialisation helpers (§14, cms.bin) ─────────────────────────────────

    /// Serialise the sketch to bytes with a 12-byte header:
    /// `[magic(4)] [total_events(8)] [last_run_ts(8)] [counters(…)]`
    ///
    /// The CRC32 checksum is prepended by `StateStore` (§14).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12 + self.counters.len() * 4);
        buf.extend_from_slice(b"LCMS");                       // magic
        buf.extend_from_slice(&self.total_events.to_le_bytes());
        buf.extend_from_slice(&self.last_run_ts.to_le_bytes());
        for &c in &self.counters {
            buf.extend_from_slice(&c.to_le_bytes());
        }
        buf
    }

    /// Deserialise from bytes produced by `to_bytes`.
    ///
    /// Returns `None` if the magic bytes do not match (triggers state rebuild).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 20 || &bytes[..4] != b"LCMS" {
            return None;
        }
        let total_events = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
        let last_run_ts  = i64::from_le_bytes(bytes[12..20].try_into().ok()?);
        let counter_bytes = &bytes[20..];
        if counter_bytes.len() != CMS_WIDTH * CMS_DEPTH * 4 {
            return None;
        }
        let counters = counter_bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        Some(Self { counters, total_events, last_run_ts })
    }

    // ── Internal hash ─────────────────────────────────────────────────────────

    /// Compute the column index for `id` in the given `row`.
    ///
    /// Uses a Knuth multiplicative hash with a different multiplier per row
    /// to approximate pairwise independence across rows (§8.1).
    #[inline]
    fn col(&self, id: TemplateId, row: usize) -> usize {
        // Different Knuth multipliers per row for hash independence.
        const MULTIPLIERS: [u64; CMS_DEPTH] = [
            0x9e3779b97f4a7c15,
            0x6c62272e07bb0142,
            0x94d049bb133111eb,
            0xbf58476d1ce4e5b9,
            0xe37e28c8bb4ec5ea,
        ];
        let h = id.wrapping_mul(MULTIPLIERS[row]);
        (h >> 44) as usize % CMS_WIDTH
    }
}

impl Default for CountMinSketch {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CountMinSketch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CountMinSketch")
            .field("total_events", &self.total_events)
            .field("last_run_ts",  &self.last_run_ts)
            .field("counters_len", &self.counters.len())
            .finish()
    }
}
