use std::io::{self, BufWriter, Write};
use std::time::Instant;

use anyhow::Context;

use crate::cli::Args;
use crate::clusterer::ShardedDrain;
use crate::compressor::Compressor;
use crate::error::Result;
use crate::formatter::{Formatter, FormatterInput};
use crate::ingest::{self, detect::InputMode};
use crate::json::JsonDocAnalyzer;
use crate::parser::{types::RunStats, MultilineConfig, Parser};
use crate::scorer::{cms::CountMinSketch, Scorer};
use crate::state::StateStore;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Primary pipeline entry point — called from `main` with parsed CLI arguments.
///
/// Dispatches to `run_log_mode` or `run_json_mode` after mode detection.
pub fn run(args: Args) -> Result<()> {
    // ── Handle `--reset-state` before touching any input ─────────────────────
    if args.reset_state {
        if let Some(slug) = effective_project_slug(&args) {
            let store = StateStore::open(&slug)?;
            store.reset()?;
        }
        return Ok(());
    }

    // ── Open input ────────────────────────────────────────────────────────────
    let source = ingest::open_input(args.file.as_deref())?;
    let mode = ingest::detect::probe_mode(source.peek_buf(), args.mode);

    tracing::debug!(?mode, "input mode detected");

    match mode {
        InputMode::JsonDocument => run_json_mode(args, source),
        InputMode::Log | InputMode::NdjsonLog => run_log_mode(args, source, mode),
    }
}

// ── Log mode ──────────────────────────────────────────────────────────────────

/// Full pipeline for line-delimited log input (log + NDJSON modes).
///
/// Pipeline stages (§3 architecture diagram):
/// ```text
/// IngestSource → Parser → [crossbeam channel] → ShardedDrain → merge
///     → Scorer → Compressor → Formatter → stdout
/// ```
///
/// TODO(§4.2, §7.2): Wire up rayon parallel workers that each read a chunk,
/// parse lines, and feed into `ShardedDrain`. Currently the stub drives the
/// pipeline single-threaded for correctness verification.
pub fn run_log_mode(
    args: Args,
    source: ingest::IngestSource,
    mode: InputMode,
) -> Result<()> {
    let t0 = Instant::now();

    // ── Load state ────────────────────────────────────────────────────────────
    let (cms, _dict) = load_state(&args)?;

    // ── Create pipeline components ────────────────────────────────────────────
    let drain = ShardedDrain::from_args(&args);
    let scorer = Scorer::new(cms);
    let compressor = Compressor::new(args.bytes_per_token, args.tokens);
    let formatter = Formatter::new(args.format, args.bytes_per_token, args.tokens, args.verbose);
    let multiline = MultilineConfig::from_char(args.multiline_start);

    let mut stats = RunStats::default();

    // ── Read all input bytes ──────────────────────────────────────────────────
    let data = source.into_bytes().context("reading input")?;
    stats.total_bytes = data.len() as u64;

    // ── Detect format and parse lines ─────────────────────────────────────────
    // TODO(§4.2): replace with rayon parallel chunk processing.
    // TODO(§5.1): sample first 1,000 lines for format detection.
    let parser = Parser::new(multiline);
    let mut byte_offset: u64 = 0;

    let (record_tx, record_rx) = crossbeam_channel::bounded::<crate::parser::types::OwnedLogRecord>(8192);

    // Parse thread stub — in the final implementation this runs in a rayon pool.
    // For the boilerplate we iterate synchronously to keep the borrow checker happy.
    {
        let line_iter = ingest::LineIter::new(&data);
        for line in line_iter {
            let (record, _raw) = parser.parse_line(line, byte_offset);
            let owned = record.to_owned();
            byte_offset += line.len() as u64 + 1;
            stats.total_lines += 1;
            // Send to drain. Channel bounded at 8192 to bound memory.
            let _ = record_tx.send(owned);
        }
        drop(record_tx); // signal EOF to the drain side
    }

    // ── Cluster ───────────────────────────────────────────────────────────────
    // TODO(§7.2): this should run concurrently with parsing via rayon.
    for record in record_rx {
        drain.insert(&record);
    }
    let templates = drain.finalise();

    // ── Score ─────────────────────────────────────────────────────────────────
    let scored = scorer.score_and_rank(templates);

    // ── Compress ──────────────────────────────────────────────────────────────
    let entries = compressor.compress(scored);

    // ── Format ────────────────────────────────────────────────────────────────
    stats.elapsed_ms = t0.elapsed().as_millis() as u64;
    let formatter_input = FormatterInput { entries, json_summary: None, stats };

    let mut sink = open_sink(&args)?;
    formatter.render(formatter_input, &mut sink)?;
    sink.flush().context("flushing output")?;

    Ok(())
}

// ── JSON document mode ────────────────────────────────────────────────────────

/// Full pipeline for single-JSON-document input (§6).
///
/// The `JsonDocAnalyzer` runs first, simultaneously:
/// 1. Building a `SchemaTree` for the structural header (§6.4).
/// 2. Emitting `OwnedLogRecord`s for all unstructured text fields (§6.3).
///
/// The emitted records flow through the same `Drain → Scorer → Compressor`
/// pipeline as log mode (§3 architecture diagram).
///
/// TODO(§6.2): Implement sonic-rs tape-mode streaming inside `JsonDocAnalyzer`.
pub fn run_json_mode(args: Args, source: ingest::IngestSource) -> Result<()> {
    let t0 = Instant::now();

    let (cms, _dict) = load_state(&args)?;

    let drain = ShardedDrain::from_args(&args);
    let scorer = Scorer::new(cms);
    let compressor = Compressor::new(args.bytes_per_token, args.tokens);
    let formatter = Formatter::new(args.format, args.bytes_per_token, args.tokens, args.verbose);

    let data = source.into_bytes().context("reading JSON document")?;
    let source_name = args
        .file
        .as_ref()
        .and_then(|p| p.to_str())
        .unwrap_or("<stdin>")
        .to_string();

    let (record_tx, record_rx) = crossbeam_channel::bounded::<crate::parser::types::OwnedLogRecord>(8192);

    let analyzer = JsonDocAnalyzer::from_args(&args);
    // Run the analyzer — emits OwnedLogRecords through record_tx.
    // In the final implementation this runs in a dedicated thread.
    let json_summary = analyzer.analyze(&data, &source_name, &record_tx)?;
    drop(record_tx);

    // Cluster the extracted text-field records.
    for record in record_rx {
        drain.insert(&record);
    }
    let templates = drain.finalise();

    let scored = scorer.score_and_rank(templates);
    let entries = compressor.compress(scored);

    let mut stats = RunStats::default();
    stats.total_bytes = data.len() as u64;
    stats.elapsed_ms  = t0.elapsed().as_millis() as u64;

    let formatter_input = FormatterInput {
        entries,
        json_summary: Some(json_summary),
        stats,
    };

    let mut sink = open_sink(&args)?;
    formatter.render(formatter_input, &mut sink)?;
    sink.flush().context("flushing output")?;

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve the effective project slug for state persistence.
///
/// If `--project` is provided, use it. Otherwise, hash the current working
/// directory to produce a stable slug.
pub fn effective_project_slug(args: &Args) -> Option<String> {
    if args.no_state {
        return None;
    }
    let slug = args.project.clone().unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = ahash::AHasher::default();
            cwd.hash(&mut h);
            h.finish()
        };
        format!("{:016x}", hash)
    });
    Some(slug)
}

/// Load the CMS and Zstd dictionary from state, if state persistence is enabled.
fn load_state(args: &Args) -> Result<(CountMinSketch, Option<Vec<u8>>)> {
    if args.no_state {
        return Ok((CountMinSketch::new(), None));
    }
    let slug = match effective_project_slug(args) {
        Some(s) => s,
        None    => return Ok((CountMinSketch::new(), None)),
    };
    let store = StateStore::open(&slug)?;
    let cms  = store.load_cms()?.unwrap_or_default();
    let dict = if args.retrain_dict { None } else { store.load_dict()? };
    Ok((cms, dict))
}

/// Open the output sink (stdout or a file).
fn open_sink(args: &Args) -> Result<Box<dyn Write>> {
    match &args.output {
        None => Ok(Box::new(BufWriter::new(io::stdout()))),
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating output file: {}", path.display()))?;
            Ok(Box::new(BufWriter::new(file)))
        }
    }
}
