use std::io::Write;

use crate::cli::OutputFormat;
use crate::compressor::CompressedEntry;
use crate::json::JsonDocSummary;
use crate::parser::types::RunStats;
use crate::scorer::Promotion;

// ── Formatter input ───────────────────────────────────────────────────────────

/// Everything the `Formatter` needs to produce final output (§10).
pub struct FormatterInput {
    /// Scored and compressed template groups.
    pub entries: Vec<CompressedEntry>,
    /// JSON structural summary — `Some` only in JSON-document mode (§6.7).
    pub json_summary: Option<JsonDocSummary>,
    /// Aggregate run statistics for `--verbose` mode.
    pub stats: RunStats,
}

// ── Formatter ─────────────────────────────────────────────────────────────────

/// Renders `FormatterInput` to the configured output sink (§10).
///
/// Delegates to a format-specific sub-renderer depending on `OutputFormat`:
/// - `Text`  → `render_text`
/// - `Json`  → `render_ndjson`
/// - `Human` → `render_human`
/// - `Raw`   → `render_raw`
pub struct Formatter {
    format:          OutputFormat,
    bytes_per_token: f64,
    token_budget:    Option<u64>,
    verbose:         bool,
}

impl Formatter {
    pub fn new(
        format:          OutputFormat,
        bytes_per_token: f64,
        token_budget:    Option<u64>,
        verbose:         bool,
    ) -> Self {
        Self { format, bytes_per_token, token_budget, verbose }
    }

    /// Render `input` to `sink`.
    ///
    /// Returns the number of bytes written.
    pub fn render<W: Write>(
        &self,
        input: FormatterInput,
        sink: &mut W,
    ) -> crate::error::Result<u64> {
        match self.format {
            OutputFormat::Text  => self.render_text(input, sink),
            OutputFormat::Json  => self.render_ndjson(input, sink),
            OutputFormat::Human => self.render_human(input, sink),
            OutputFormat::Raw   => self.render_raw(input, sink),
        }
    }

    // ── Text renderer (§10.2) ─────────────────────────────────────────────────

    /// Plain-text output optimised for LLM context windows.
    ///
    /// Layout:
    /// ```text
    /// # JSON Document: <source_name>          ← if json_summary present
    /// <schema tree>                            ← if json_summary present
    ///
    /// ## Log Patterns
    ///
    /// [NEW]      Failed to connect to * (count: 47)
    ///   vars: *=[127.0.0.1, 10.0.0.1, …]
    /// [ANOMALY]  Timeout after *ms (count: 1234)
    ///   …
    /// (42 normal patterns omitted for brevity)
    ///
    /// ## Run Stats
    /// lines: 1,234,567 | elapsed: 2.1s | templates: 89
    /// ```
    ///
    /// TODO(§10.2): full text renderer with token-budget enforcement.
    fn render_text<W: Write>(
        &self,
        input: FormatterInput,
        sink: &mut W,
    ) -> crate::error::Result<u64> {
        let _ = (input, sink);
        todo!("§10.2: plain-text renderer")
    }

    // ── NDJSON renderer (§10.3) ───────────────────────────────────────────────

    /// NDJSON output — one JSON object per entry, one per line.
    ///
    /// TODO(§10.3): implement NDJSON renderer using `serde_json`.
    fn render_ndjson<W: Write>(
        &self,
        input: FormatterInput,
        sink: &mut W,
    ) -> crate::error::Result<u64> {
        let _ = (input, sink);
        todo!("§10.3: NDJSON renderer")
    }

    // ── Human / ANSI renderer (§10.4) ─────────────────────────────────────────

    /// ANSI-coloured output with a progress bar (§10.4).
    ///
    /// TODO(§10.4): use `indicatif` for the progress bar and ANSI escape codes
    /// for promotion-tier colouring.
    fn render_human<W: Write>(
        &self,
        input: FormatterInput,
        sink: &mut W,
    ) -> crate::error::Result<u64> {
        let _ = (input, sink);
        todo!("§10.4: human/ANSI renderer")
    }

    // ── Raw renderer ──────────────────────────────────────────────────────────

    /// Raw output: one line per parsed record with field annotations.
    ///
    /// Useful for debugging and downstream tool piping.
    fn render_raw<W: Write>(
        &self,
        input: FormatterInput,
        sink: &mut W,
    ) -> crate::error::Result<u64> {
        let _ = (input, sink);
        todo!("§10: raw renderer")
    }
}

// ── Promotion colouring (§10.4) ───────────────────────────────────────────────

/// ANSI colour codes for promotion tiers in `--format=human` output.
pub fn promotion_color(p: Promotion) -> &'static str {
    match p {
        Promotion::Novelty  => "\x1b[1;35m", // bold magenta
        Promotion::Anomaly  => "\x1b[1;31m", // bold red
        Promotion::Elevated => "\x1b[1;33m", // bold yellow
        Promotion::Normal   => "\x1b[0m",    // reset
    }
}

pub const ANSI_RESET: &str = "\x1b[0m";
