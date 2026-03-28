use clap::{Parser, ValueEnum};
use std::path::PathBuf;

// ── Output format ─────────────────────────────────────────────────────────────

/// Controls the shape of lumen's output (§10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Condensed plain text optimised for LLM context windows (default).
    Text,
    /// NDJSON — one object per template group, for tool-calling agents.
    Json,
    /// ANSI-coloured output with a progress bar, for human terminals.
    Human,
    /// One parsed line per record; useful for debugging or further piping.
    Raw,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Text
    }
}

// ── Processing mode ───────────────────────────────────────────────────────────

/// Override lumen's automatic input-mode detection (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModeOverride {
    /// Auto-detect from the first 4 KB of input (default).
    Auto,
    /// Treat input as a line-delimited log stream or NDJSON.
    Log,
    /// Treat input as a single JSON document (object or array).
    Json,
}

impl Default for ModeOverride {
    fn default() -> Self {
        Self::Auto
    }
}

// ── Top-level argument struct ─────────────────────────────────────────────────

/// `lumen` — high-performance log and JSON condenser for LLMs.
#[derive(Debug, Parser)]
#[command(
    name    = "lumen",
    version,
    about   = "Transform large log files and JSON documents into LLM-ready summaries.",
    long_about = None,
)]
pub struct Args {
    // ── Input / output ────────────────────────────────────────────────────────

    /// Input file. Omit or pass '-' to read from stdin.
    #[arg(value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Write output to FILE instead of stdout.
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Output format.
    #[arg(short = 'f', long = "format", value_enum, default_value = "text")]
    pub format: OutputFormat,

    /// Cap output at approximately N tokens.
    #[arg(long, value_name = "N")]
    pub tokens: Option<u64>,

    /// Bytes-per-token ratio for --tokens estimation (§18.4).
    #[arg(long, default_value = "4.0", value_name = "F")]
    pub bytes_per_token: f64,

    // ── Mode & project identity ────────────────────────────────────────────────

    /// Processing mode override (§6.1).
    #[arg(long, value_enum, default_value = "auto")]
    pub mode: ModeOverride,

    /// Project slug for state persistence. Defaults to a hash of the working directory.
    #[arg(long, value_name = "SLUG")]
    pub project: Option<String>,

    // ── Resource limits ────────────────────────────────────────────────────────

    /// Worker thread count. Defaults to the number of logical CPUs.
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,

    /// Ingestion chunk size in bytes (§4.1).
    #[arg(long, default_value = "16777216", value_name = "BYTES")]
    pub chunk_size: usize,

    /// Abort if RSS exceeds this limit in MB (§15).
    #[arg(long, default_value = "512", value_name = "MB")]
    pub memory_limit: u64,

    // ── Log-mode tuning (§7.4) ────────────────────────────────────────────────

    /// Drain Jaccard similarity threshold for cluster matching.
    #[arg(long, default_value = "0.5", value_name = "F")]
    pub sim_threshold: f64,

    /// Drain max children per prefix-tree node before wildcarding.
    #[arg(long, default_value = "128", value_name = "N")]
    pub max_children: usize,

    /// Drain prefix-tree depth.
    #[arg(long, default_value = "4", value_name = "N")]
    pub depth: usize,

    /// Minimum occurrence count before a template is clustered (not emitted verbatim).
    #[arg(long, default_value = "2", value_name = "N")]
    pub min_cluster_size: usize,

    /// Line-start character for multiline log folding (§18.1).
    /// When set, a new record begins every time a line starts with this character.
    #[arg(long, value_name = "CHAR")]
    pub multiline_start: Option<char>,

    // ── Scorer / CMS tuning (§8, §18.3) ──────────────────────────────────────

    /// Number of historic runs kept in the Count-Min Sketch.
    #[arg(long, default_value = "10", value_name = "N")]
    pub history_runs: usize,

    /// CMS time-weighted decay half-life in hours (§18.3). Default = 1 week.
    #[arg(long, default_value = "168", value_name = "HOURS")]
    pub cms_half_life: f64,

    // ── State persistence ─────────────────────────────────────────────────────

    /// Disable all state persistence (CMS, Zstd dict, meta.json).
    #[arg(long)]
    pub no_state: bool,

    /// Delete project state files and exit without processing input.
    #[arg(long)]
    pub reset_state: bool,

    /// Force Zstd dictionary retraining on this run (§9.3).
    #[arg(long)]
    pub retrain_dict: bool,

    // ── JSON document mode (§6) ───────────────────────────────────────────────

    /// Focus output on the subtree at DOTPATH (e.g. "results[].detail").
    #[arg(long, value_name = "DOTPATH")]
    pub json_path: Option<String>,

    /// Maximum schema tree depth before truncating subtrees (§6.6).
    #[arg(long, default_value = "12", value_name = "N")]
    pub max_depth: usize,

    /// Number of array elements to sample for schema inference (§6.5).
    #[arg(long, default_value = "3", value_name = "N")]
    pub max_array_samples: usize,

    /// Emit all elements verbatim for arrays with ≤ N items (§6.5).
    #[arg(long, default_value = "20", value_name = "N")]
    pub max_array_inline: usize,

    /// Emit the schema tree only; suppress all sampled values and examples.
    #[arg(long)]
    pub schema_only: bool,

    /// Shannon entropy threshold above which a string is classified as Scalar,
    /// filtering out JWTs, base64, and UUIDs (§18.10).
    #[arg(long, default_value = "3.5", value_name = "F")]
    pub entropy_threshold: f64,

    // ── Verbosity ─────────────────────────────────────────────────────────────

    /// Print pipeline statistics to stderr after processing.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Suppress all stderr output including warnings.
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,
}
