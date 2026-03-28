pub mod cms;

use crate::clusterer::template::{LogTemplate, TemplateId};
use cms::CountMinSketch;

// ── Promotion tier ────────────────────────────────────────────────────────────

/// The novelty/anomaly classification for a template, based on its surprise
/// score relative to the historic Count-Min Sketch (§8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Promotion {
    /// Surprise < 1.0 bits — expected behaviour.
    Normal,
    /// 1.0 ≤ surprise < 3.0 bits — slightly more frequent than usual.
    Elevated,
    /// Surprise ≥ 3.0 bits — significantly more frequent than usual.
    Anomaly,
    /// Template has never been seen before (`expected_freq == 0`).
    /// Always emitted at the top of output with `[NEW]` tag (§8.3, §10.2).
    Novelty,
}

impl Promotion {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Normal   => "",
            Self::Elevated => "[ELEVATED]",
            Self::Anomaly  => "[ANOMALY]",
            Self::Novelty  => "[NEW]",
        }
    }

    pub fn is_highlighted(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

// ── Surprise scoring (§8.2) ───────────────────────────────────────────────────

/// Compute the information-gain "surprise" score for a template.
///
/// ```text
/// expected_freq  = cms.estimate(id) / total_historic_events
/// observed_freq  = count             / total_current_events
/// surprise       = max(0, log2(observed_freq / expected_freq))
/// ```
///
/// Returns `f64::INFINITY` if `expected_freq == 0` (never seen before).
pub fn compute_surprise(
    cms: &CountMinSketch,
    template_id: TemplateId,
    count: u64,
    total_current: u64,
) -> f64 {
    if total_current == 0 {
        return 0.0;
    }
    let expected_raw = cms.estimate(template_id);
    let total_historic = cms.total_events;

    if expected_raw == 0 || total_historic == 0 {
        return f64::INFINITY;
    }

    let expected_freq = expected_raw as f64 / total_historic as f64;
    let observed_freq = count as f64 / total_current as f64;
    let ratio = observed_freq / expected_freq;

    if ratio <= 1.0 {
        0.0
    } else {
        ratio.log2()
    }
}

/// Map a surprise score to a `Promotion` tier (§8.3).
pub fn score_to_promotion(surprise: f64) -> Promotion {
    if surprise.is_infinite() {
        Promotion::Novelty
    } else if surprise >= 3.0 {
        Promotion::Anomaly
    } else if surprise >= 1.0 {
        Promotion::Elevated
    } else {
        Promotion::Normal
    }
}

// ── Scored template ───────────────────────────────────────────────────────────

/// A `LogTemplate` decorated with its surprise score and promotion tier,
/// ready for the `Compressor` and `Formatter`.
pub struct ScoredTemplate {
    pub template:   LogTemplate,
    pub surprise:   f64,
    pub promotion:  Promotion,
    /// Historic frequency estimate from the CMS (for display: "usual: ~N/run").
    pub historic_estimate: u32,
}

impl ScoredTemplate {
    pub fn new(template: LogTemplate, surprise: f64, historic_estimate: u32) -> Self {
        let promotion = score_to_promotion(surprise);
        Self { template, surprise, promotion, historic_estimate }
    }
}

impl std::fmt::Debug for ScoredTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScoredTemplate")
            .field("id",        &self.template.id)
            .field("pattern",   &self.template.pattern())
            .field("count",     &self.template.occurrence_count())
            .field("promotion", &self.promotion)
            .field("surprise",  &self.surprise)
            .finish()
    }
}

// ── Scorer ────────────────────────────────────────────────────────────────────

/// Assigns surprise scores and promotion tiers to all templates from a run (§8).
pub struct Scorer {
    cms: CountMinSketch,
}

impl Scorer {
    /// Create a scorer from a loaded (or fresh) `CountMinSketch`.
    pub fn new(cms: CountMinSketch) -> Self {
        Self { cms }
    }

    /// Score all templates from the current run and sort them by promotion
    /// (Novelty first, then Anomaly, then by occurrence count descending).
    pub fn score_and_rank(&self, templates: Vec<LogTemplate>) -> Vec<ScoredTemplate> {
        let total_current: u64 = templates.iter().map(|t| t.occurrence_count()).sum();

        let mut scored: Vec<ScoredTemplate> = templates
            .into_iter()
            .map(|t| {
                let surprise = compute_surprise(&self.cms, t.id, t.occurrence_count(), total_current);
                let historic = self.cms.estimate(t.id);
                ScoredTemplate::new(t, surprise, historic)
            })
            .collect();

        // Sort: Novelty > Anomaly > Elevated > Normal; ties broken by count desc.
        scored.sort_by(|a, b| {
            b.promotion
                .cmp(&a.promotion)
                .then_with(|| b.template.occurrence_count().cmp(&a.template.occurrence_count()))
        });

        scored
    }

    /// Consume the scorer and return the underlying CMS for persistence.
    pub fn into_cms(self) -> CountMinSketch {
        self.cms
    }

    /// Get a reference to the CMS (for `StateStore` to persist it).
    pub fn cms(&self) -> &CountMinSketch {
        &self.cms
    }

    /// Update the CMS with the current run's template counts, applying
    /// time-weighted decay first (§18.3).
    pub fn flush_to_cms(
        &mut self,
        run_counts: &[(TemplateId, u64)],
        now_ts: i64,
        half_life_hours: f64,
    ) {
        self.cms.apply_decay(now_ts, half_life_hours);
        self.cms.merge_run(run_counts, now_ts);
    }
}
