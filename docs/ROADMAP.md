# Lumen — Spec Completion Roadmap

**Audience:** Solo maintainer, ~2–3 hours per weekend.  
**Source of truth:** `SPEC.md` (v0.1.0-draft).  
**Purpose:** Turn the current Rust skeleton into a shippable implementation that satisfies the full specification, without losing the “bounded output from unbounded input” contract.

---

## How to use this document

- **Tickets** are sized for a **single weekend session (2–3 hours)** when marked **S**. **M** means plan for two weekends or one longer session. **L** is a multi-week epic split across several tickets.
- **Dependencies** matter: later milestones assume earlier ones are at least minimally working (especially the end-to-end path: parse → cluster → score → compress → format → state).
- **Definition of done** for the whole project: `cargo test` and representative golden tests pass; log mode and JSON document mode run on large fixtures without `todo!` panics; CLI flags in §11 behave as specified (or are explicitly reconciled with the spec).

---

## Current state (honest snapshot)

**Already in place (good foundations):**

- Project layout, `clap` CLI with most flags from §11, mode probing for JSON document vs NDJSON vs plain log (`src/ingest/detect.rs`).
- Ingestion: mmap / small file / stdin, transparent `.gz` / `.zst` / `.bz2`, SIMD newline scan via `memchr` (`src/ingest/mod.rs`).
- Core data types: `LogRecord` / `OwnedLogRecord`, `RecordSource`, template types (`src/parser/types.rs`, `src/clusterer/template.rs`).
- Count–Min Sketch with time-weighted decay (§8.1, §18.3), surprise scoring and promotion tiers (`src/scorer/`).
- State store: `cms.bin` with CRC32, `dict.zst`, simplified `meta.json` (`src/state/mod.rs`).
- JSON text classification and entropy filter scaffolding (`src/json/classify.rs`); schema tree structs (`src/json/schema.rs`).

**Blocking gaps (the binary does not complete a full run today):**

- `FormatHint::detect` and several parser paths are `todo!`; the pipeline never calls `detect_format`, so structured parsing is not active.
- `DrainShard::insert` is `todo!` — clustering does not run.
- `Compressor::compress` and all formatter renderers are `todo!`.
- `JsonDocAnalyzer::analyze` is `todo!` — JSON document mode is unimplemented.
- Shard merge pass after clustering is stubbed (`merge_pass`).
- Post-run persistence: CMS load exists, but merging/flushing CMS + `meta.json` after a successful run is not wired in `pipeline::run_*`.
- Several spec items are partial placeholders: HyperLogLog / t-digest in schema, scavenger eviction, Zstd dictionary training and use, streaming chunked ingestion for huge files, rayon-parallel parse/drain, second-pass `json-path` with `[*]`.

This roadmap closes those gaps in an order that keeps the main pipeline working as early as possible.

---

## Milestones

| Milestone | Goal | Spec coverage (high level) |
|-----------|------|-----------------------------|
| **M1 — Vertical slice** | One full run: ingest → parse (at least raw) → Drain → score → compress → text output → exit 0 | §3, §5 (raw), §7 (core), §8 (score), §9–10 (minimal), §11 |
| **M2 — Parser & multiline** | Tiered format detection, simd-json NDJSON, logfmt/heuristics fallbacks, multiline folding | §5, §18.1 |
| **M3 — Clusterer production** | Complete Drain insert, `--min-cluster-size`, parallel shard execution + merge §18.2, cross-path `source_paths` §18.11 | §7, §18.2, §18.11 |
| **M4 — Compressor & output** | Slot stats, semantic deltas §9.2, token budget §10.3, all output modes | §9–10, §18.4 |
| **M5 — State & dictionary** | Reliable CMS flush, full `meta.json` §14, Zstd dict train/retrain policy §9.3 | §8–9, §14 |
| **M6 — Ingestion scale** | Chunked/streaming pipeline, bounded channels, optional large-file strategy §4.1, memory guard §15 | §4, §12, §15 |
| **M7 — JSON document mode** | sonic-rs tape streaming, schema trie with HLL + t-digest, text extraction & sibling promotion §6 | §6, §18.6–18.7 |
| **M8 — JSON advanced** | `--json-path` subtree + `[*]` leaf scan §18.8, depth limits §6.6, scavenger §18.9, entropy SIMD §18.10 | §6.6, §18.8–18.11 |
| **M9 — Quality & performance** | Tests §16, benchmarks, fuzzing, CI load target, platform notes §18.5 | §12, §16–17 |

---

## Ticket backlog

### Theme A — Unblock the vertical slice (M1)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **A1** | ✅ Complete | Implement `DrainShard::insert` (tokenise, prefix tree walk, Jaccard match, new cluster, `max_children` wildcarding) | §7.1–7.4 | **L** | Split across A1a/A1b if needed: (1) tokenise + tree navigation + new template; (2) similarity update + wildcard rules. Must respect `depth`, `sim_threshold`, `max_children`. |
| **A2** |✅ Complete | Wire `Parser::detect_format` in `run_log_mode`: sample first 1000 non-empty lines, set format hint | §5.1 | **M** | Until A3–A6 exist, detection can return `Raw` after scaffolding; must not panic. |
| **A3** | ⏳ Pending | Replace `FormatHint::detect` `todo!` with heuristic scoring (JSON line ratio, `=`, syslog PRI, CLF patterns) | §5.1 | **M** | Lock format after sample; per-line fallback to raw as spec states. |
| **A4** | ✅ Complete | Minimal `Compressor::compress`: map `ScoredTemplate` → `CompressedEntry` (pattern string, count, promotion, empty slots) | §9.1 (minimal) | **S** | No slot statistics yet; enough for formatter to print. |
| **A5** | ✅ Complete | Implement `Formatter::render_text` for log mode per §10.2 (header, sections NEW/ANOMALY/NORMAL, truncation line) | §10.2 | **M** | Match structure in spec example; include run stats line when `verbose`. |
| **A6** | ✅ Complete | Wire post-run: `extract_run_counts` → `Scorer::flush_to_cms` → `StateStore::save_cms` + `save_meta` (extend meta to match §14 schema if needed) | §8, §14 | **M** | Use wall-clock `now` for decay; skip if `--no-state`. |
| **A7** | ✅ Complete | End-to-end integration test: small fixture log → non-empty text output, no panic | §16 | **S** | Golden file optional in A7; can be follow-up. |

**Dependency note:** A1 is complete. A4–A5 can now be stubbed in parallel once templates list is non-empty.

---

### Theme B — Parser depth (M2)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **B1** | ⏳ Pending | NDJSON path: `simd-json` parse per line into `LogRecord` (timestamp/level/message/fields) with borrow from line buffer | §5.3 | **M** | Parse valid NDJSON without allocation-heavy DOM materialisation; fall back to raw on malformed lines without panicking. |
| **B2** | ⏳ Pending | Logfmt / key=value SIMD-friendly scanner for `=` and delimiters | §5.3 | **M** | Extract message plus up to configured key/value fields; preserve raw line and tolerate quoted values / missing values conservatively. |
| **B3** | ⏳ Pending | Common Log Format + RFC5424 syslog parsers (structured tier) | §5.1 | **M** | Recognise CLF and RFC5424 samples during format detection; populate timestamp/level/message where present and raw fallback otherwise. |
| **B4** | ⏳ Pending | Heuristic tier: timestamp regex/state machine, severity keywords (integrate with existing `Level::from_bytes`) | §5.1 | **M** | Strip common timestamp/severity prefixes into structured fields; keep original raw line and avoid false positives on plain text. |
| **B5** | ⏳ Pending | Multiline: implement `fold_multiline` and integrate `MultilineConfig` in line iteration (indent vs `--multiline-start`) | §18.1 | **M** | Stack traces / continuation lines become one record; explicit `--multiline-start` takes precedence over indentation heuristic. |
| **B6** | ⏳ Pending | Track `RunStats.unparseable_lines` and `verbose` stats for fallthrough lines | §5.1, §15 | **S** | Increment counters consistently across parser fallbacks; verbose text output surfaces totals without changing quiet mode. |

---

### Theme C — Clusterer completion (M3)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **C1** | ⏳ Pending | Implement `merge_pass`: group by token count, edit-distance-1 merge, combine counts, examples, CMS identity §18.2 | §18.2 | **M** | Converge templates that differ by one token into wildcard templates; preserve combined counts, timestamps, paths, and examples. |
| **C2** | ⏳ Pending | Honour `--min-cluster-size`: emit verbatim / separate handling for rare lines per §7.4 | §7.4 | **S** | Rare templates below threshold are not folded into misleading clusters; output path remains deterministic for small fixtures. |
| **C3** | ⏳ Pending | Parallel ingestion: rayon workers parsing chunks + `ShardedDrain::insert` (bounded channel) | §7.2, §3 | **L** | Parallel path matches single-threaded golden output; channel bounds memory and no shard contention hot spot dominates throughput. |
| **C4** | ⏳ Pending | Shard key + JSON path: ensure `RecordSource::JsonField` populated from JSON mode (prep for M7) | §7.2, §18.11 | **S** | JSON-extracted records carry stable paths through parsing, clustering, compression, and formatter input. |
| **C5** | ⏳ Pending | `LogTemplate` examples: maintain up to 3 representatives; evict policy after cluster phase if spec requires | §7.3, §12 | **S** | Each template retains at most three useful raw examples without unbounded memory growth. |

---

### Theme D — Compressor & formatter (M4)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **D1** | ⏳ Pending | Slot statistics from wildcard positions: numeric min/max/p50/p99; string sets ≤20 else distinct count | §9.1 | **M** | Populate `VariableSlot` for wildcard positions using observed examples/matches; distinguish numeric and string distributions. |
| **D2** | ⏳ Pending | Semantic deltas within same template (relative timestamps / field deltas) | §9.2 | **M** | Repeated template runs show meaningful deltas instead of redundant examples; output remains compact and deterministic. |
| **D3** | ⏳ Pending | `--tokens` enforcement: always emit Novelty+Anomaly; fill with Normal by count; truncation footer | §10.3 | **M** | Respect configured token budget approximately; never drop Novelty/Anomaly and report omitted entry counts. |
| **D4** | ⏳ Pending | `Formatter::render_ndjson`: serde schema for one object per template group | §10.1 | **M** | Emit one valid JSON object per line with stable field names covering template, count, promotion, stats, paths, and examples. |
| **D5** | ⏳ Pending | `Formatter::render_human`: ANSI + `indicatif` progress (spinner or bar during read) | §10.1 | **M** | Human mode adds colour/progress without corrupting text/JSON modes; disable/avoid noisy progress for non-interactive sinks. |
| **D6** | ⏳ Pending | `Formatter::render_raw`: one line per record with parsed fields (debug) | §10.1 | **S** | Debug output exposes parser decisions line-by-line for fixtures; useful for validating parser stages. |
| **D7** | ⏳ Pending | JSON document text output layout: `JsonDocSummary` header per §6.7 (structure block + cluster sections) | §6.7 | **M** | JSON mode output includes source header, schema summary, and extracted log pattern sections in the same text format. |
| **D8** | ⏳ Pending | Multi-path template rendering §18.11 (`paths:` with per-path counts) | §18.11 | **S** | Templates shared across JSON paths show contributing paths and counts without duplicating the pattern body. |

---

### Theme E — State & Zstd (M5)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **E1** | ⏳ Pending | Align `meta.json` with §14 (`schema_version`, `runs[]` with `run_id`, `template_count`, etc.) | §14 | **M** | Replace simplified metadata with versioned schema; load older state safely or rebuild with clear warning. |
| **E2** | ⏳ Pending | `templates.bin` persistence (optional for MVP; spec lists it — define minimal binary or defer with doc) | §14 | **M** | Either implement stable template persistence or explicitly defer it with rationale and compatibility notes. |
| **E3** | ⏳ Pending | Dictionary training: sample up to 100 MB raw text, `zstd::dict::from_samples`, `save_dict` | §9.3 | **M** | Train from bounded raw samples and persist `dict.zst`; skip cleanly when input/state constraints prevent training. |
| **E4** | ⏳ Pending | Use trained dict for compressing CMS output / intermediate buffers where spec applies | §9.3 | **S** | Existing dictionaries are loaded and applied only where beneficial; missing/corrupt dict falls back without failing runs. |
| **E5** | ⏳ Pending | Retrain triggers: after 50 runs or cosine distance > 0.3 between CMS histograms (define histogram representation) | §9.3 | **M** | Retraining decision is deterministic, documented in metadata, and avoids excessive retrain churn. |
| **E6** | ⏳ Pending | CLI parity pass: spec lists `--decay`; code uses `--cms-half-life` — add alias or document single source of truth | §11 vs §18.3 | **S** | CLI help and spec agree; old flag names remain user-friendly if aliases are added. |

---

### Theme F — Ingestion & memory (M6)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **F1** | ⏳ Pending | Avoid full `into_bytes()` for multi-GB files: stream chunks with `chunk_size`, feed `LineIter` per chunk with offset tracking | §4.1 | **L** | Large files are processed without full materialisation; record byte offsets remain correct across chunk boundaries. |
| **F2** | ⏳ Pending | Back-pressure: bounded channels between stages; document buffer sizes | §3 | **M** | Pipeline stages use bounded queues with clear capacities; slow consumers do not cause unbounded memory growth. |
| **F3** | ⏳ Pending | `--memory-limit`: monitor RSS (platform-specific), reduce chunk size or skip dict training when near limit | §15 | **M** | Memory guard acts before OOM; degraded modes are logged and output remains valid. |
| **F4** | ⏳ Pending | Time to first output byte: structure pipeline so formatter can start after first batch (target §12) | §12 | **M** | Streaming architecture can emit useful partial output before the whole input is consumed where format allows. |

---

### Theme G — JSON document core (M7)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **G1** | ⏳ Pending | sonic-rs tape-mode: stream document, emit tokens without materialising DOM | §6.2 | **L** | JSON documents are walked as a stream/tape; large inputs avoid DOM-sized memory spikes. |
| **G2** | ⏳ Pending | Large document windows: 32 MB overlapping windows with tape stitch | §6.2 | **M** | Window boundaries preserve valid token context; large fixtures produce the same summary as smaller in-memory paths. |
| **G3** | ⏳ Pending | Build `SchemaTree` during tape walk: paths, type counts, nodes | §6.4 | **M** | Schema tree records paths, observed types, counts, and truncation flags while respecting max depth. |
| **G4** | ⏳ Pending | Replace `CardinalitySketch` placeholder with HyperLogLog++ (14-bit); wire `insert`/`estimate` | §6.4 | **M** | Cardinality estimates are stable enough for schema summaries and covered by focused tests. |
| **G5** | ⏳ Pending | Replace `NumericStats` placeholder with streaming t-digest for p50/p99 | §6.4 | **M** | Numeric summaries report min/max/p50/p99 without storing all values. |
| **G6** | ⏳ Pending | Array summarisation: count, sample positions (first/middle/last up to `max_array_samples`), inline ≤ `max_array_inline` | §6.5 | **M** | Arrays show count and representative samples; small arrays can be inlined according to CLI limits. |
| **G7** | ⏳ Pending | Reservoir sampling (200) for internal schema stats when array larger than samples | §18.6 | **M** | Large-array sampling remains bounded and representative enough for schema/type summaries. |
| **G8** | ⏳ Pending | Text classifier: wire `classify_string`; extract `UnstructuredLine` / `UnstructuredMultiline` to `OwnedLogRecord` | §6.3 | **M** | Text-like JSON strings become records for the shared log pipeline; scalar/noise strings are filtered out. |
| **G9** | ⏳ Pending | Parse extracted strings with shared `Parser`; attach `json_path`, `json_index`, sibling scalars | §6.3 | **M** | Extracted JSON text reuses parser logic and carries origin metadata through clustering. |
| **G10** | ⏳ Pending | Multiline JSON strings: unescape, split on `\n`, fold with §18.1 heuristic | §6.3 | **M** | Multiline JSON log strings produce coherent records, not one noisy record per physical line. |
| **G11** | ⏳ Pending | Polymorphic keys & type variance annotations (common / partial / delta footnote) | §18.7 | **M** | Schema output marks type/key variance clearly without overwhelming common-case structure. |
| **G12** | ⏳ Pending | `--schema-only` behaviour | §6, §11 | **S** | Schema-only mode suppresses extracted log clusters/examples while still emitting valid schema summary. |

---

### Theme H — JSON advanced (M8)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **H1** | ⏳ Pending | `--json-path` subtree focus: filter schema + extraction to rooted path | §6.6 | **M** | Selected subtree limits both schema output and text extraction; invalid paths produce clear errors or empty summaries. |
| **H2** | ⏳ Pending | Path expression parser: `.key`, `[N]`, `[*]` | §18.8 | **M** | Parser accepts documented path grammar, rejects ambiguous input, and has tests for nested arrays/objects. |
| **H3** | ⏳ Pending | Second-pass leaf scan for `[*]` wildcard: full value distribution at leaf | §18.8 | **M** | Wildcard array focus reports leaf distributions across all matching elements without scanning unrelated branches. |
| **H4** | ⏳ Pending | Depth truncation message with key count hint | §6.6 | **S** | Truncated schema nodes include a concise count/hint so users know output is intentionally bounded. |
| **H5** | ⏳ Pending | Schema trie scavenger at 90 MB / 100 MB ceiling; eviction policy §18.9 | §18.9 | **L** | Memory ceiling triggers deterministic eviction/summarisation and preserves high-value/common schema paths. |
| **H6** | ⏳ Pending | SIMD entropy histogram (or `wide` / arch intrinsics) for §18.10 fast path | §18.10 | **M** | Entropy classifier gets a measured speedup or documented fallback while preserving classification results. |
| **H7** | ⏳ Pending | Configurable `--entropy-threshold` fully applied (already partially wired) | §18.10 | **S** | CLI threshold changes classification decisions consistently across JSON extraction paths. |

---

### Theme I — Hardening & release (M9)

| ID | Status | Title | Spec | Size | Notes / acceptance |
|----|--------|-------|------|------|---------------------|
| **I1** | ⏳ Pending | Unit tests: scorer formulas, CMS decay, surprise boundaries | §16 | **S** | Cover surprise thresholds, Novelty/Anomaly promotion, CMS estimate, and decay edge cases. |
| **I2** | ⏳ Pending | Property tests: `proptest` for CMS monotonicity / parser invariants | §16 | **M** | Randomised tests assert CMS counters do not undercount inserted IDs and parser never panics on arbitrary bytes. |
| **I3** | ⏳ Pending | Golden integration tests: fixtures → expected condensed output | §16 | **M** | Stable fixtures cover log modes and representative output sections; intentional output changes update goldens deliberately. |
| **I4** | ⏳ Pending | `cargo-fuzz` targets for parser + Drain | §16 | **M** | Fuzz targets exercise parser/tokeniser/Drain insert paths with crashers minimised and checked in when useful. |
| **I5** | ⏳ Pending | Criterion benches per stage (`benches/pipeline.rs` extension) | §16 | **M** | Benchmarks isolate ingest, parse, cluster, score, compress, and format stages with reproducible fixture sizes. |
| **I6** | ⏳ Pending | CI: large synthetic log job on main (1 GB or scaled-down proxy with same code paths) | §16 | **M** | CI exercises the same large-input code path within practical runtime limits and catches regressions. |
| **I7** | ⏳ Pending | `cargo audit` in CI; document `unsafe` SAFETY review | §13 | **S** | Security audit runs automatically; every `unsafe` block has an adjacent SAFETY rationale. |
| **I8** | ⏳ Pending | Windows non-x86_64 compile-time warning; document Linux/macOS SIMD | §18.5 | **S** | Platform notes are explicit and unsupported SIMD assumptions fail loudly or fall back safely. |
| **I9** | ⏳ Pending | SIGPIPE: verify flush-on-break for piped output (Unix); document Windows behaviour | §15 | **S** | Piped output terminates cleanly when downstream closes; Windows behaviour is documented and tested where feasible. |

---

## Implementation timeline (all tickets)

**Assumptions:** ~2–3 hours per weekend session; **S** ≈ 1 weekend of focused work, **M** ≈ 2 weekends, **L** ≈ 3+ weekends (split across consecutive slots). Tickets on the same bullet may be done in parallel if you have extra time. **Seq** is the recommended global order when a single thread of work; respect **Depends on** before starting a ticket.

### Phase 0 — Delivered (M1 foundation)

| Seq | Ticket | Size | Notes |
|-----|--------|------|--------|
| 0a | **A1** | L | Drain insert — done. |
| 0b | **A2** | M | `detect_format` wired; stub `Raw` ok until **A3**. — done. |

### Phase 1 — Finish vertical slice (M1)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 1 | **A4** | S | A1 | Minimal `Compressor::compress`; unblocks formatter input. |
| 2 | **A5** | M | A4 | `render_text` for log mode. |
| 3 | **A6** | M | A5 | Post-run CMS + `meta` flush; use wall-clock decay. |
| 4 | **A7** | S | A6 | First end-to-end integration test (fixture → text, no panic). |
| 5 | **E6** | S | — | CLI `--decay` vs `--cms-half-life` parity; reduces spec drift while state work is fresh. |

### Phase 2 — Parser tier (M2)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 6 | **A3** | M | A2 | Real `FormatHint::detect` heuristics; lock after sample. |
| 7 | **B6** | S | A3 | `RunStats` / verbose for fallthrough; validates detection quality. |
| 8 | **B5** | M | A7 | Multiline folding in line iteration; enables realistic fixtures. |
| 9 | **B1** | M | B5 | NDJSON / `simd-json` path. |
| 10 | **B2** | M | B5 | Logfmt scanner. |
| 11 | **B3** | M | B1, B2 | CLF + RFC5424 structured parsers. |
| 12 | **B4** | M | B3 | Heuristic timestamp / severity tier. |

### Phase 3 — Clusterer production (M3)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 13 | **C1** | M | A7 | `merge_pass` §18.2. |
| 14 | **C2** | S | C1 | `--min-cluster-size` / rare lines. |
| 15 | **C5** | S | C1 | Template examples (≤3 reps). |
| 16 | **C4** | S | C2 | `RecordSource::JsonField` for JSON path story. |

### Phase 4 — Compressor & formatter depth (M4, log path)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 17 | **D1** | M | C1 | Slot statistics from wildcards. |
| 18 | **D2** | M | D1 | Semantic deltas §9.2. |
| 19 | **D3** | M | A5 | `--tokens` budget §10.3. |
| 20 | **D4** | M | D3 | `render_ndjson`. |
| 21 | **D6** | S | D4 | `render_raw` debug. |
| 22 | **D8** | S | D4 | Multi-path rendering §18.11. |
| 23 | **D5** | M | D4 | `render_human` + progress UI. |

### Phase 5 — State & dictionary (M5)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 24 | **E1** | M | A6 | `meta.json` ↔ §14. |
| 25 | **E3** | M | E1 | Train Zstd dict from samples. |
| 26 | **E4** | S | E3 | Use dict where spec applies. |
| 27 | **E5** | M | E4 | Retrain triggers. |
| 28 | **E2** | M | E1 | `templates.bin` (or explicit defer + doc). |

### Phase 6 — JSON document core (M7)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 29 | **G1** | L | A7 | Tape-mode spike; largest schedule risk. |
| 30 | **G2** | M | G1 | 32 MB overlapping windows. |
| 31 | **G3** | M | G2 | `SchemaTree` during walk. |
| 32 | **G4** | M | G3 | HLL++ cardinality. |
| 33 | **G5** | M | G3 | t-digest numeric stats. |
| 34 | **G6** | M | G3 | Array summarisation §6.5. |
| 35 | **G7** | M | G6 | Reservoir sampling §18.6. |
| 36 | **G8** | M | G3 | Text classifier → `OwnedLogRecord`. |
| 37 | **G9** | M | G8, B1 | Shared `Parser` on extracted strings + JSON path metadata. |
| 38 | **G10** | M | G9 | Multiline JSON strings + fold. |
| 39 | **G11** | M | G3 | Polymorphic keys §18.7. |
| 40 | **G12** | S | G3 | `--schema-only`. |
| 41 | **D7** | M | G9, G11 | JSON document text layout §6.7 (after extraction path exists). |

### Phase 7 — JSON advanced (M8)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 42 | **H2** | M | G9 | Path expression parser (prerequisite for **H1**/**H3**). |
| 43 | **H1** | M | H2 | `--json-path` subtree focus. |
| 44 | **H4** | S | H1 | Depth truncation messaging. |
| 45 | **H3** | M | H2 | `[*]` second-pass leaf scan. |
| 46 | **H6** | M | G8 | SIMD entropy §18.10. |
| 47 | **H7** | S | H6 | Wire `--entropy-threshold` fully. |
| 48 | **H5** | L | G3, G7 | Schema scavenger §18.9; do after core trie is stable. |

### Phase 8 — Ingestion scale (M6)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 49 | **F1** | L | C1 | Chunked ingestion; avoid full-buffer `into_bytes()` for huge files. |
| 50 | **F2** | M | F1 | Bounded channels / back-pressure §3. |
| 51 | **F3** | M | F1 | `--memory-limit` / RSS guard §15. |
| 52 | **F4** | M | F2 | Time to first output byte §12. |

### Phase 9 — Parallelism after correctness (M3/M6)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 53 | **C3** | L | I3, C1 | Rayon parse + drain; only after single-threaded output matches goldens. |

### Phase 10 — Quality, release, CI (M9)

| Seq | Ticket | Size | Depends on | Notes |
|-----|--------|------|------------|--------|
| 54 | **I1** | S | A7 | Unit tests: scorer, CMS decay, surprise. |
| 55 | **I2** | M | I1 | `proptest` CMS / parser invariants. |
| 56 | **I3** | M | A7 | Golden integration tests. |
| 57 | **I4** | M | B1, C1 | `cargo-fuzz` parser + Drain. |
| 58 | **I5** | M | C3 | Criterion benches per stage. |
| 59 | **I6** | M | F1 | CI large-log job (full size or proxy path). |
| 60 | **I7** | S | — | `cargo audit`; `unsafe` review notes. |
| 61 | **I8** | S | — | Windows / SIMD platform notes §18.5. |
| 62 | **I9** | S | A5 | SIGPIPE / piped output behaviour §15. |

**Parallel quality track:** **I1** can start as soon as **A7** lands; **I2**–**I3** follow **I1**. **I4** is best after parser + Drain are stable. **I5**–**I6** assume pipeline and ingestion paths exist. The table assigns **I*** after **C3**/ingestion in sequence, but in calendar time you should overlap **I1**–**I3** with Phases 2–5 wherever possible.

### Approximate calendar span

Summing phase roughness: Phases 1–5 ≈ **6–9 months** of weekends at ~2.5 h/week; Phases 6–8 (JSON + scale) ≈ **9–15 months** additional; Phase 9–10 overlap partially. **Total to full backlog:** on the order of **18–30 months** of solo weekend cadence — treat as a long arc, not a deadline.

---

## Spec coverage checklist (traceability)

Use this to confirm nothing important is dropped:

| Spec section | Themes |
|--------------|--------|
| §4 Ingestion | F1–F4, A (line SIMD already partial) |
| §5 Parser | A2–A3, B1–B6 |
| §6 JSON mode | G1–G12, H1–H7, D7 |
| §7 Drain | A1, C1–C5 |
| §8 Scorer | A6, E1, E5 |
| §9 Compressor | A4, D1–D2, E3–E5 |
| §10 Formatter | A5, D3–D8 |
| §11 CLI | E6, flags already mostly present; add any missing (e.g. `--bytes-per-token` already present) |
| §12 Performance | C3, F1–F4, I5–I6 |
| §13 Dependencies/unsafe | I7 |
| §14 State files | A6, E1–E2 |
| §15 Errors / SIGPIPE / OOM | B6, F3, I9 |
| §16 Testing | I1–I6 |
| §17 Versioning | Release process (document in release ticket) |
| §18 Design details | B5, C1, D8, G7, G11, H2–H6, E5, E6 |

---

## Risks & decisions

- **JSON document mode** is the largest schedule risk (tape streaming + schema + extraction). Defer **H** until **G** produces correct clustered output for medium fixtures.
- **Parallelism** vs correctness: parallelise only after single-threaded outputs match golden files.
- **Spec vs code drift:** `meta.json` shape and `--decay` naming should be reconciled early (**E6**) to avoid user confusion.

---

*This roadmap is a living backlog. When a ticket completes, add a one-line note in git history or your issue tracker pointing at the PR — future you will thank present you.*
