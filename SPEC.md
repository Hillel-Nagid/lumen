# Lumen — Specification

**Version:** 0.1.0-draft  
**Target Language:** Rust (stable, 2021 edition)  
**Primary Audience:** LLM tool-calling pipelines, CI log analysis, on-call engineers

---

## 1. Purpose

Lumen is a high-performance CLI tool that transforms arbitrarily large text files — log streams, NDJSON logs, and large structured JSON documents — into a compressed, semantically dense representation that fits within an LLM context window. It is designed first and foremost to be called by LLMs (via tool use, shell invocation, or piped output) but is equally useful for human operators.

The core contract: **given unlimited input, produce bounded, prioritised, human-readable output.**

Lumen operates in one of two modes, selected automatically or overridden with `--mode`:

| Mode | Trigger | Description |
|---|---|---|
| **Log mode** | Line-delimited text, NDJSON | Cluster, deduplicate, score, and condense log streams |
| **JSON Document mode** | Single JSON object or array | Infer schema, sample values, summarise structure for debugging |

---

## 2. Non-Goals

- Lumen is not a log storage engine or indexer.
- Lumen does not perform root-cause analysis — it prepares context so an LLM can.
- Lumen does not require logs to follow any particular format or schema.
- Lumen does not replace structured logging systems (Loki, Splunk, etc.).

---

## 3. Architecture Overview

```
stdin / file
     │
     ▼
┌─────────────┐     memory-mapped or streaming reader
│  Ingestion  │     (zero-copy, SIMD byte scanning)
└──────┬──────┘
       │
       ▼
┌─────────────┐     inspect first 4 KB — line-delimited vs. JSON document
│ Mode Detect │
└──────┬──────┘
       │
       ├──── JSON document ───────────────────────────────────────────────────┐
       │                                                                       │
       ▼  (Log / NDJSON)                                                       ▼
┌─────────────┐  SIMD field extractor              ┌────────────────────────────────┐
│   Parser    │  timestamp/level/message/k=v        │   JSON Doc Analyzer            │
└──────┬──────┘                                     │   ├─ streaming tape parse      │
       │  LogRecord stream                          │   ├─ schema inference          │
       │                                            │   ├─ array sampling            │
       │                                            │   └─ text field extraction     │
       │                                            └──────────┬─────────────────────┘
       │                                                        │
       │                              ┌─────────────────────────┤
       │                              │                         │
       │                    JsonDocSummary             LogRecord stream
       │                    (structural schema view)   (from extracted text fields)
       │                              │                         │
       └──────────────────────────────┼─────────────────────────┘
                                      │  unified LogRecord stream
                                      ▼
                          ┌──────────────────────┐
                          │  Clusterer (Drain)    │  parallel sharded, rayon pool
                          └──────────┬───────────┘
                                     │  (TemplateID, variable slots)
                                     ▼
                          ┌──────────────────────┐
                          │  Scorer              │  Count-Min Sketch, novelty
                          └──────────┬───────────┘
                                     │
                                     ▼
                          ┌──────────────────────┐
                          │  Compressor          │  Zstd dict-trained, semantic deltas
                          └──────────┬───────────┘
                                     │
                                     ▼
                          ┌──────────────────────────────────────────┐
                          │  Formatter                               │
                          │  JSON mode: structural summary header    │
                          │            + cluster / scored output     │
                          │  Log mode: cluster / scored output only  │
                          └──────────────────────────────────────────┘
```

Every stage is pipelined with bounded, fixed-size channels so that memory usage stays constant regardless of input size.

---

## 4. Ingestion

### 4.1 Reading Strategy

| Input Source | Strategy |
|---|---|
| Regular file | `mmap` via the `memmap2` crate; kernel handles paging |
| Stdin / pipe | 4 MB ring-buffer reads; avoids copies via `std::io::BufReader` with a custom buffer |
| Compressed file (`.gz`, `.zst`, `.bz2`) | Transparent decompression via `async-compression` or `flate2` / `zstd` |

Files under 64 MB are read into a single contiguous buffer. Files above 64 MB are chunked into 16 MB segments and fed to the pipeline concurrently.

### 4.2 SIMD Line Finding

Line boundaries are located using SIMD intrinsics (via the `memchr` crate, which automatically selects SSE2/AVX2/NEON at compile time) rather than byte-at-a-time scanning.

```
Throughput target: ≥ 3 GB/s line scanning on a modern x86-64 core
```

Each identified line is handed off as a `&[u8]` slice — no allocation, no copy.

---

## 5. Parser

### 5.1 Goals

The parser must handle **any text file** structure. It uses a tiered detection strategy:

1. **Structured tier** — detect well-known formats (JSON, logfmt, Common Log Format, syslog RFC5424).
2. **Heuristic tier** — detect timestamp patterns, severity keywords, and key=value pairs using a hand-written state machine compiled with SIMD character classification.
3. **Raw tier** — treat the entire line as an opaque message string; no fields extracted.

Format detection runs on the first 1,000 lines and is locked for the remainder of the file, but any line that fails to parse under the detected format falls through to the raw tier.

### 5.2 LogRecord

```rust
pub struct LogRecord<'buf> {
    pub timestamp:   Option<i64>,          // Unix microseconds, or None
    pub level:       Option<Level>,        // Trace/Debug/Info/Warn/Error/Fatal
    pub message:     &'buf [u8],           // borrowed from input buffer
    pub fields:      SmallVec<[Field<'buf>; 8]>,
    pub raw_line:    &'buf [u8],
    pub byte_offset: u64,
    pub source:      RecordSource<'buf>,   // origin of this record
}

pub enum RecordSource<'buf> {
    /// Came directly from a line in the input (log mode).
    LogLine,
    /// Extracted from a JSON string value at the given path (JSON document mode).
    JsonField { path: &'buf str },
}

pub struct Field<'buf> {
    pub key:   &'buf [u8],
    pub value: &'buf [u8],
}
```

`SmallVec` is used to keep the struct on the stack for up to 8 fields — the common case — with heap fallback for richer structured logs. `RecordSource` carries the JSON path for records extracted from JSON documents; the Clusterer uses this to shard and the Formatter uses it to group cluster output by path.

### 5.3 SIMD Field Extraction

For JSON lines: use `simd-json` (port of simdjson) for zero-copy parsing.  
For logfmt / key=value: use a SIMD-accelerated scanner that finds `=` and delimiter characters in 16-byte or 32-byte lanes.

---

## 6. JSON Document Mode

This mode activates when lumen detects that the input is a single JSON object (`{`) or JSON array (`[`) rather than a stream of lines. It is designed for debugging any large, deeply-nested JSON payload — regardless of its origin, schema, or purpose — that is too large to paste into an LLM prompt. Lumen makes no assumptions about what the document represents.

JSON Document mode does **not** bypass the core pipeline. It runs alongside it:

1. The `JSON Doc Analyzer` streams through the document, building a structural summary and classifying every string value.
2. Scalar strings (IDs, enums, timestamps) feed into schema statistics only.
3. **Unstructured text blobs** (long strings, embedded stack traces, log messages stored as field values) are extracted as `LogRecord`s and fed directly into the shared **Drain → Scorer → Compressor** pipeline, exactly as if they were lines in a log file.
4. The `Formatter` emits the structural summary as a header, followed by the clustered, novelty-scored, compressed view of all extracted text.

### 6.1 Mode Detection

Mode detection reads the first 4 KB of input and applies the following heuristic in order:

1. Skip leading whitespace/BOM.
2. If the first non-whitespace byte is `{` or `[` → **JSON Document mode**.
3. If ≥ 80% of the first 200 non-empty lines are valid JSON objects → **NDJSON Log mode** (existing pipeline).
4. Otherwise → **Log mode** (existing pipeline).

Override with `--mode log` or `--mode json`.

### 6.2 Streaming Tape Parser

Lumen uses `sonic-rs` in tape (event) mode rather than deserialising into a Rust value tree. This means the document is **never fully materialised in memory** — the parser emits a flat sequence of typed tokens (`ObjectStart`, `Key`, `Str`, `U64`, `ArrayStart`, `ArrayEnd`, etc.) which the analyser consumes as a stream.

For documents larger than the chunk size (default 16 MB), the file is parsed in overlapping 32 MB windows with the tape stitched across window boundaries.

```
Memory target (JSON document mode): ≤ 64 MB RSS regardless of input size
```

### 6.3 Text Field Extraction

As the tape stream is consumed, every `Str` token is passed through the **Text Field Classifier** before being added to schema statistics:

```rust
pub enum TextClass {
    /// Short string, low entropy — ID, enum value, boolean-like.
    /// Used for schema cardinality stats only.
    Scalar,
    /// Long string with word boundaries. Treat as a single log message.
    UnstructuredLine,
    /// String containing embedded '\n' (JSON-escaped). May be a multi-line
    /// stack trace, exception dump, or concatenated log blob.
    UnstructuredMultiline,
}
```

**Classification rules** (applied in order):

| Condition | Class |
|---|---|
| Length < 60 bytes OR no ASCII space | `Scalar` |
| Contains `\n` (byte `0x0A` after JSON unescaping) | `UnstructuredMultiline` |
| Starts with a timestamp pattern OR contains level keyword | `UnstructuredLine` |
| Length ≥ 60 bytes AND word count ≥ 5 | `UnstructuredLine` |
| Otherwise | `Scalar` |

**`UnstructuredLine` → `LogRecord` extraction:**

The string value is parsed by the standard Parser (Section 5) to extract any timestamp, level, and key=value pairs embedded within it. The JSON path context is attached as additional synthetic fields:

```rust
// A string value at results[42].event.description becomes:
LogRecord {
    message:   <the string value>,
    fields:    [
        Field { key: b"json_path",  value: b"results[].event.description" },
        Field { key: b"json_index", value: b"42" },
        // plus any scalar siblings at the same object level:
        Field { key: b"level",     value: b"ERROR" },
        Field { key: b"region",    value: b"eu-west-1" },
    ],
    timestamp: <from a sibling field whose value parses as a timestamp, if any>,
    ..
}
```

Sibling field promotion: when extracting a text value, the analyzer walks back up the path trie to collect all scalar siblings at the same object level and attaches them as fields on the `LogRecord`. This ensures the cluster output is contextualized — e.g., "this template appeared 400 times, all with region=eu-west-1."

**`UnstructuredMultiline` → folded `LogRecord`:**

The string is first JSON-unescaped, then split on `\n`. The resulting lines are folded using the multiline heuristic defined in Section 18.1 to produce a single `LogRecord` whose `message` is the first line and whose `raw_line` is the full block. This handles embedded stack traces:

```json
{ "error": "NullPointerException: Cannot invoke foo()\n\tat com.example.Svc.handle(Svc.java:42)\n\tat ..." }
```

→ folded into one `LogRecord` with `message = "NullPointerException: Cannot invoke foo()"` and the full block preserved in `raw_line`.

### 6.4 Schema Inference

As the tape stream is consumed, lumen builds a `SchemaTree` — a path-keyed trie where each node represents a JSON path segment:

```rust
pub struct SchemaNode {
    pub path:        Arc<str>,                   // e.g. "results[].event.level"
    pub type_counts: EnumMap<JsonType, u64>,     // Null/Bool/Int/Float/Str/Obj/Arr
    pub cardinality: HyperLogLog,                // approximate distinct value count
    pub numeric:     Option<NumericStats>,        // min, max, p50, p99 (t-digest)
    pub samples:     ArrayVec<[SmolStr; 5]>,     // up to 5 sampled string values
    pub array_len:   Option<NumericStats>,        // if this node is under an array
    pub child_count: u64,                        // total times this path was visited
}
```

- **HyperLogLog** (14-bit, ≈ 0.8% error) for cardinality estimation without storing values.
- **t-digest** for numeric quantile estimation in a single streaming pass.
- Array elements are treated as a single schema path with `[]` notation; the analyser samples up to `--max-array-samples` (default 3) representative elements at positions first, middle, last.

### 6.5 Array Summarisation

Arrays are the primary source of size in large JSON documents. Rather than emitting every element, lumen:

1. Counts total elements.
2. Infers the **element schema** by sampling up to `--max-array-samples` elements spread evenly across the array (positions: first, evenly-spaced middle, last).
3. Emits a compact block:

```
"results": [ ×10,000 items ]
  element schema: {
    "id":        string    10,000 distinct
    "region":    string    3 distinct: ["eu-west-1", "us-east-1", "ap-south-1"]
    "severity":  string    4 distinct: ["CRITICAL", "HIGH", "MEDIUM", "LOW"]
    "score":     float     range=[0.12, 0.98]  p50=0.71  p99=0.97
    "message":   string    9,847 distinct  → extracted as text (see §6.3)
    "detail": {
      "duration_ms": integer  range=[0, 45,221]  p50=18  p99=8,200
      "trace":       string   312 distinct    → extracted as text (multiline)
    }
  }
  [sample #1]     { "id": "a3f", "severity": "CRITICAL", "message": "Connection refused", ... }
  [sample #5000]  { "id": "b7c", "severity": "HIGH",     "message": "Slow query detected", ... }
  [sample #10000] { "id": "c1d", "severity": "LOW",      "message": "Request completed", ... }
```

If the array has ≤ `--max-array-inline` (default 20) elements, all elements are emitted verbatim.

### 6.6 Depth Limiting and Path Filtering

Very deeply nested documents (depth > `--max-depth`, default 12) have subtrees replaced by:

```
[depth limit reached — subtree has N keys, use --json-path "foo.bar.baz" to expand]
```

The `--json-path <DOTPATH>` flag focuses the entire output on a subtree rooted at the given path, using the same dot-bracket notation as the schema tree (e.g. `results[].detail`). This allows an LLM to iteratively drill down into a large document:

```bash
# First pass: get structure overview
lumen payload.json

# Second pass: focus on a specific subtree
lumen --json-path "results[].detail" payload.json
```

### 6.7 JSON Document Output Example

```
═══════════════════════════════════════════════════════════
LUMEN JSON SUMMARY  source=payload.json (2.1 GB)
Schema depth: 6  │  Text fields extracted: 10,000  │  Processed in 4.2s
Templates: 47  │  Novel: 1  │  Anomalous: 2
═══════════════════════════════════════════════════════════

── STRUCTURE ──────────────────────────────────────────────
{
  "meta": {
    "generated_at": "2026-03-28T14:55:01Z"
    "count":        10,000
    "version":      "3.1.0"
  }
  "results": [ ×10,000 items ]
    element schema: {
      "id":       string    10,000 distinct
      "region":   string    3 distinct: ["eu-west-1", "us-east-1", "ap-south-1"]
      "severity": string    4 distinct: ["CRITICAL", "HIGH", "MEDIUM", "LOW"]
      "score":    float     range=[0.12, 0.98]  p50=0.71
      "message":  string    9,847 distinct  → extracted as text (see below)
      "detail": {
        "duration_ms": integer  range=[0, 45,221]  p50=18  p99=8,200
        "trace":       string   312 distinct    → extracted as text (multiline)
      }
    }
    [sample #1]     { "id": "a3f", "severity": "CRITICAL", "message": "Connection refused on port *", ... }
    [sample #5000]  { "id": "b7c", "severity": "HIGH",     "message": "Slow query detected on table *", ... }
    [sample #10000] { "id": "c1d", "severity": "LOW",      "message": "Request completed in *ms", ... }
  "summary": {
    "by_severity": {
      "CRITICAL": 312
      "HIGH":     1,844
      "MEDIUM":   5,100
      "LOW":      2,744
    }
  }
}

── [NEW] NOVEL TEXT PATTERNS ──────────────────────────────
  path: results[].detail.trace  severity=CRITICAL
[NEW] [×1] OutOfMemoryError: Java heap space
    OutOfMemoryError: Java heap space
        at com.example.Worker.processItem(Worker.java:318)
        at ...

── [ANOMALY] ELEVATED TEXT PATTERNS ───────────────────────
  path: results[].message  region=eu-west-1  (usual: ~3/run)
[ANOMALY] [×312] Failed to verify token for issuer=*
    issuer: (3 distinct: ["sso.internal", "legacy-idp", "ext-provider"])

── TEXT FIELD CLUSTERS ─────────────────────────────────────
  path: results[].message
[×4,821] Connected to * in *ms
    (47 distinct hosts)  latency_p50=18ms  latency_p99=8,200ms
[×3,100] HTTP * * /api/v1/* responded in *ms
    status: 3 distinct: [200, 404, 500]  latency_p99=410ms
[×1,244] Slow query on table=* took *ms
    (8 distinct tables)  duration_p99=8,200ms
... (43 more templates) ...

  path: results[].detail.trace  [311 distinct folded traces]
[×200] NullPointerException at *.*(*.java:*)
[×80]  TimeoutException: Read timed out after *ms
[×31]  (other exception templates)
═══════════════════════════════════════════════════════════
```

---

## 7. Clusterer (Drain Algorithm)

### 7.1 Drain Overview

Drain is a streaming log-template miner that works without seeing the full dataset. It maintains an internal prefix tree where:

- Interior nodes are **fixed tokens** (literal words that appear consistently).
- Leaf nodes are **LogCluster** objects containing a **template** (with `*` wildcards for variable positions) and an **occurrence count**.

### 7.2 Parallel Drain

Single-threaded Drain is a bottleneck for high-volume logs. Lumen implements a **sharded parallel Drain**:

1. Incoming `LogRecord`s are hashed by `(source_path, level, message_word_count)` to a shard index. Records from the same JSON path always land on the same shard, so templates are naturally grouped by field origin.
2. Each shard owns an independent Drain tree and a `Mutex<DrainShard>`.
3. A `rayon` thread pool drives the shards concurrently.
4. After all lines are processed, shard trees are **merged**: identical templates are unified, variable counts summed.

```
Shard count: num_cpus × 2 (bounded at 128)
Lock contention target: < 5% of wall time
```

### 7.3 Template Representation

```rust
pub struct LogTemplate {
    pub id:       TemplateId,          // u64 stable hash of the token pattern
    pub tokens:   Vec<Token>,          // Literal(str) | Wildcard
    pub count:    AtomicU64,
    pub first_seen: i64,               // Unix µs
    pub last_seen:  i64,
    pub examples: ArrayVec<[LogRecord; 3]>, // up to 3 representative records
}
```

### 7.4 Tuning Parameters

| Parameter | Default | Description |
|---|---|---|
| `--sim-threshold` | `0.5` | Jaccard similarity threshold for grouping into existing cluster |
| `--max-children` | `128` | Max children per prefix-tree node before wildcarding |
| `--depth` | `4` | Max prefix-tree depth |
| `--min-cluster-size` | `2` | Templates with fewer hits are emitted verbatim |

---

## 8. Scorer (Novelty Detection)

### 8.1 Count-Min Sketch

A Count-Min Sketch (CMS) tracks approximate frequency of `TemplateId`s across the **last N project runs** (default N=10). The CMS is stored in a compact binary file in the project state directory.

```
State directory: $XDG_STATE_HOME/lumen/<project-slug>/  (Linux)
                 %LOCALAPPDATA%\lumen\<project-slug>\   (Windows)
                 ~/Library/Application Support/lumen/<project-slug>/ (macOS)
```

CMS parameters:
- Width: 2^20 (≈1M counters per row)
- Depth: 5 hash functions
- Counter width: 32-bit (saturating)
- Total memory: 20 MB per project state

### 8.2 Surprise Scoring

For each `LogTemplate` observed in the current run:

```
expected_freq  = cms.estimate(template.id)  /  total_historic_events
observed_freq  = template.count             /  total_current_events
surprise       = max(0, log2(observed_freq / expected_freq))
```

Templates with `expected_freq == 0` (never seen before) receive `surprise = ∞` and are classified as **Novelties**.

### 8.3 Promotion Rules

| Score | Classification | Output Position |
|---|---|---|
| ∞ (new template) | **Novelty** | Top of output, `[NEW]` tag |
| ≥ 3.0 bits | **Anomaly** | After novelties, `[ANOMALY]` tag |
| 1.0 – 3.0 bits | **Elevated** | Normal position, `[ELEVATED]` tag |
| < 1.0 bits | **Normal** | Normal position |

After a successful run, the CMS is updated with the current run's template frequencies using an exponential decay on old counts (decay factor α = 0.9 per run, configurable).

---

## 9. Compressor

### 9.1 Condensed Text Format

The compressor replaces high-frequency, low-surprise template repetitions with a compact summary:

```
[×12,847] [INFO] Connected to database at * in *ms
          └─ last seen: 2026-03-28T14:22:01Z  delta_p50: 4ms  delta_p99: 312ms
```

For templates with variable slots, the compressor extracts the distribution of variable values:

- **Numeric slots**: emit min, max, p50, p99.
- **String slots with low cardinality** (≤ 20 distinct): emit the value set.
- **String slots with high cardinality**: emit `(N distinct values)`.

This is the **lossy** step. The original log lines are not recoverable from this output.

### 9.2 Semantic Deltas

Within a run of the same template, only changed field values are emitted:

```
[14:20:00] WARN  Retry attempt 1/3 for job=abc-123 host=db-01
[+120ms]          ↳ attempt=2/3
[+240ms]          ↳ attempt=3/3  [FAILED]
```

### 9.3 Zstd Dictionary Training

On the **first run** for a new project slug, lumen samples up to 100 MB of raw log data and trains a Zstd compression dictionary via `zstd::dict::from_samples`. The dictionary (typically 100–500 KB) is saved to the state directory.

On **subsequent runs**, all intermediate buffers and the final output are compressed using this trained dictionary. This raises the effective compression ratio by 20–60% over generic Zstd for repetitive, project-specific log vocabularies (GUIDs, service names, stack frames).

The dictionary is automatically retrained after 50 runs or when the template cluster distribution diverges significantly (cosine distance > 0.3 between CMS histograms).

---

## 10. Formatter / Output

### 10.1 Output Modes

| Flag | Format | Audience |
|---|---|---|
| *(default)* | Plain condensed text | LLMs |
| `--json` | NDJSON one object per template-group | Tool-calling agents |
| `--human` | ANSI-coloured, aligned, with progress bar | Terminal users |
| `--raw` | One line per record, parsed fields only | Debugging / pipe |

### 10.2 Condensed Text Structure

```
═══════════════════════════════════════════════════
LUMEN SUMMARY  run_id=a3f8b2  source=app.log (1.2 GB, 4,821,044 lines)
Processed in 3.1s  │  Templates: 312  │  Novel: 2  │  Anomalous: 7
═══════════════════════════════════════════════════

── [NEW] NOVEL PATTERNS ──────────────────────────
[NEW] [×1] [ERROR] Connection refused on port 9200
    2026-03-28T14:55:01Z  host=es-03  job=reindex-7f2a

[NEW] [×3] [WARN] Circuit breaker OPEN for service=payments
    first: 14:50:12Z  last: 14:55:44Z

── [ANOMALY] ELEVATED PATTERNS ──────────────────
[ANOMALY] [×4,221] [WARN] Slow query: * took *ms  (usual: ~40/run)
    query_p99=8,200ms  query_p50=320ms  table=orders

── NORMAL PATTERNS ───────────────────────────────
[×12,847] [INFO] Connected to database at * in *ms
    delta_p50=4ms  delta_p99=312ms

[×9,001]  [INFO] HTTP 200 GET /api/v1/* in *ms
    path: (47 distinct)  latency_p50=22ms  latency_p99=410ms

... (308 more templates) ...
═══════════════════════════════════════════════════
```

### 10.3 Token Budget Mode

Pass `--tokens <N>` to cap the output at approximately N tokens (estimated at 4 bytes/token). Lumen will:

1. Always emit all Novelty and Anomaly sections.
2. Fill remaining budget with Normal templates ordered by occurrence count descending.
3. Append a truncation notice: `[...truncated: 201 templates omitted, total_lines=1,823,041]`.

---

## 11. CLI Interface

```
USAGE:
    lumen [OPTIONS] [FILE]

ARGS:
    [FILE]    Input file. Omit or use '-' to read from stdin.

OPTIONS:
    -o, --output <FILE>         Write output to file instead of stdout
    -f, --format <FMT>          Output format: text (default), json, human, raw
        --tokens <N>            Cap output at approximately N tokens
        --mode <MODE>           Processing mode: auto (default), log, json
        --project <SLUG>        Project identifier for state persistence [default: cwd-hash]
        --sim-threshold <F>     Drain similarity threshold [default: 0.5]
        --max-children <N>      Drain max prefix children [default: 128]
        --depth <N>             Drain prefix tree depth [default: 4]
        --history-runs <N>      CMS history window in runs [default: 10]
        --decay <F>             CMS per-run decay factor [default: 0.9]
        --no-state              Disable all state persistence (CMS, dict)
        --reset-state           Clear project state and exit
        --retrain-dict          Force Zstd dictionary retraining this run
        --threads <N>           Worker thread count [default: num_cpus]
        --chunk-size <BYTES>    Ingestion chunk size [default: 16MiB]

JSON DOCUMENT MODE OPTIONS:
        --json-path <DOTPATH>   Focus output on a subtree (e.g. "results[].detail")
        --max-depth <N>         Maximum schema tree depth before truncation [default: 12]
        --max-array-samples <N> Number of array elements to sample [default: 3]
        --max-array-inline <N>  Arrays with ≤ N elements are shown verbatim [default: 20]
        --schema-only           Emit schema tree only; omit all sampled values

    -v, --verbose               Print pipeline stats to stderr
    -q, --quiet                 Suppress all stderr output
    -h, --help                  Print help
    -V, --version               Print version
```

---

## 12. Performance Targets

**Log mode:**

| Metric | Target |
|---|---|
| Throughput (plain text, no state) | ≥ 500 MB/s on 8-core machine |
| Throughput (full pipeline, CMS + dict) | ≥ 200 MB/s on 8-core machine |
| Peak memory (1 GB input) | ≤ 256 MB RSS |
| Time to first output byte | ≤ 100 ms |
| Output size vs. input (typical app log) | ≤ 0.5% (i.e., 1 GB → ≤ 5 MB) |

**JSON Document mode:**

| Metric | Target |
|---|---|
| Throughput (schema inference, single-threaded) | ≥ 400 MB/s on modern x86-64 |
| Peak memory (any input size) | ≤ 64 MB RSS |
| Time to first output byte | ≤ 50 ms |
| Output size vs. input (typical JSON document) | ≤ 0.1% (i.e., 2 GB → ≤ 2 MB) |

Memory is kept bounded by:
- Streaming tape parsing — the document is never fully loaded.
- HyperLogLog + t-digest replace value storage with probabilistic sketches.
- Array sampling is O(samples), not O(array length).
- Streaming ingestion with back-pressure via bounded channels.
- Evicting old `LogTemplate.examples` after the clustering phase.
- Compressing the CMS state file on flush with the trained dictionary.

---

## 13. Crate Dependencies

| Crate | Purpose |
|---|---|
| `clap` (v4) | Argument parsing |
| `memchr` | SIMD line/byte finding |
| `simd-json` | Zero-copy NDJSON log parsing (log mode) |
| `sonic-rs` | SIMD tape-mode parser for JSON document mode |
| `rayon` | Data-parallel thread pool |
| `crossbeam-channel` | Bounded pipeline channels |
| `zstd` | Compression + dictionary training |
| `flate2` | Gzip transparent decompression |
| `memmap2` | Memory-mapped file reading |
| `smallvec` | Stack-allocated small vecs |
| `arrayvec` | Fixed-capacity stack vecs |
| `smol_str` | Cheap small string clones for schema node paths |
| `ahash` | Fast non-crypto hashing |
| `hyperloglogplus` | Cardinality estimation for JSON schema fields |
| `tdigest` | Streaming quantile estimation for numeric fields |
| `serde` / `serde_json` | JSON output serialisation |
| `indicatif` | Progress bar (human mode) |
| `anyhow` | Error handling |
| `tracing` | Internal diagnostics |

All dependencies must be audited with `cargo audit` before release. `unsafe` blocks are permitted wherever they provide a measurable performance or memory benefit (SIMD intrinsics, zero-copy buffer access, lock-free data structures, etc.). Every `unsafe` block must carry a `// SAFETY:` comment that justifies the invariants being upheld.

---

## 14. State File Layout

```
$STATE_DIR/<project-slug>/
├── cms.bin          Count-Min Sketch (binary, custom header + raw u32 array)
├── dict.zst         Trained Zstd dictionary
├── meta.json        Run history metadata (run count, timestamps, template counts)
└── templates.bin    Serialised template tree from last run (for diff display)
```

`meta.json` schema:

```json
{
  "schema_version": 1,
  "project_slug": "my-service",
  "runs": [
    {
      "run_id": "a3f8b2",
      "timestamp": "2026-03-28T14:55:01Z",
      "source": "app.log",
      "total_lines": 4821044,
      "total_bytes": 1288490188,
      "template_count": 312
    }
  ]
}
```

---

## 15. Error Handling

- **Unparseable lines**: silently passed to the raw tier; counted in `--verbose` stats.
- **Corrupt state files**: detected via a CRC32 header checksum; on failure, state is rebuilt from scratch with a warning to stderr.
- **OOM conditions**: lumen monitors RSS against a configurable `--memory-limit` (default 512 MB) and will reduce chunk size or skip dictionary training if the limit is approached.
- **SIGPIPE**: handled gracefully (output is flushed, process exits 0) — critical for piped LLM tool use.

---

## 16. Testing Strategy

| Layer | Approach |
|---|---|
| Unit | Pure functions (scorer, compressor formulas) via `#[test]` |
| Property | `proptest` for parser round-trips and CMS frequency bounds |
| Integration | Golden-file tests: known log fixtures → expected condensed output |
| Fuzz | `cargo-fuzz` targets for the parser and Drain tree |
| Benchmark | `criterion` benchmarks for each pipeline stage; tracked in CI |
| Load | 1 GB synthetic log (generated fixture) run in CI on every merge to main |

---

## 17. Versioning and Stability

- The condensed **text output format** (Section 9.2) is considered stable from v1.0.0. Changes require a minor version bump.
- The **JSON output format** schema is stable from v1.0.0. Additive fields are allowed in patch releases.
- The **state file formats** (CMS, dict, meta) are versioned internally. Lumen will migrate or rebuild state on version mismatch.
- The **CLI flags** follow SemVer: no removals or renames in a major series.

---

## 18. Design Decisions

### 18.1 Multiline Log Folding

Default strategy: **indentation-based**. A continuation line is any non-empty line whose first non-whitespace character is further right than the first non-whitespace character of the preceding anchor line. The anchor line becomes the `LogRecord.message`; all continuation lines are appended to `raw_line`.

An explicit **line-start character** can be provided via `--multiline-start <CHAR>` (e.g. `[` for lines beginning with a timestamp bracket). A new record begins whenever a line starts with that character; everything before the next match is folded into the current record. Regex-based continuation is explicitly not supported — the folding loop must stay allocation-free and branch-predictable.

### 18.2 Drain Shard Merge Strategy

Two templates `A` and `B` from different shards are candidates for unification if and only if:

1. They have the **same token count**.
2. **Exactly one** token position differs between them (edit distance = 1 at the token level).

When both conditions hold, a new template `C` is created where the differing position becomes a `Wildcard`. The merged template inherits:

```
C.count        = A.count + B.count
CMS(C)         = CMS(A) + CMS(B)   // frequencies summed before CMS update
C.first_seen   = min(A.first_seen, B.first_seen)
C.last_seen    = max(A.last_seen,  B.last_seen)
C.examples     = A.examples[..] + B.examples[..]  // up to 3 total
```

Templates `A` and `B` are removed from their respective shards after merging. The merge pass runs once, after all shard trees have been finalised, on a single thread (merge is O(templates²) in the worst case but typically O(templates) due to shard locality).

### 18.3 CMS Decay Model

**Time-weighted decay.** Rather than decaying by a fixed factor per run, the decay applied to historic counts is a function of elapsed wall-clock time since each run:

```
decay(run_i) = exp(-λ · Δt_i)
```

where `Δt_i` is the number of hours since run `i` and `λ` is a configurable half-life parameter (`--cms-half-life`, default `168h` = 1 week). This ensures that a burst of daily runs does not over-represent a short period, and that a long gap does not keep stale templates artificially alive.

### 18.4 Token Budget Calibration

The bytes-per-token ratio used by `--tokens` is **configurable** via `--bytes-per-token <F>` (default `4.0`). There is no automatic tokeniser detection. Users targeting a specific model should measure their model's ratio against a representative lumen output and set the flag accordingly.

### 18.5 Platform SIMD Support

Lumen supports **x86-64 only on Windows**. The SSE2/AVX2 paths in `memchr`, `simd-json`, and `sonic-rs` are used unconditionally on that target. ARM64 Windows (e.g. Surface Pro X under emulation) falls back to scalar paths. A compile-time warning is emitted on non-x86-64 Windows targets. On Linux and macOS, x86-64 and AArch64 (NEON) are both fully supported.

### 18.6 Array Reservoir Sampling

Reservoir size: **200 elements**. Standard reservoir sampling (Algorithm R) is applied when an array's element count exceeds `--max-array-samples` (default 3 for the structural view). The 200-element reservoir is used internally to build the element schema and the value statistics (HyperLogLog, t-digest); only `--max-array-samples` representative elements are emitted in the output.

### 18.7 Polymorphic Array Element Schemas

Keys are partitioned into three tiers based on their presence ratio across sampled elements:

- **Common keys** (present in > 80% of sampled elements): rendered inline as normal schema fields.
- **Polymorphic keys** (present in < 20% of sampled elements): omitted from the main schema view and moved to a **Schema Delta** footnote: `[schema delta: N keys seen in <20% of elements — use --json-path to inspect]`.
- **Type variance errors**: if a key carries conflicting types across elements (e.g. `integer` in 500 elements, `string` in 2), the minority type is flagged inline as `[TYPE VARIANCE: string in 2/500 elements]`.

Keys in the 20–80% band are rendered with an occupancy annotation: `"region": string? (61% of elements)`.

### 18.8 `--json-path` Wildcard Support

`results[*].detail.level` is a valid path expression. The `[*]` wildcard means "all elements of this array, without sampling." This triggers a dedicated **leaf-scan pass**: the tape parser streams the document a second time, visiting only the nodes matching the path expression, and collects the full value distribution for that leaf across every array element. The path expression language supports:

- `.key` — object key descent
- `[N]` — specific index
- `[*]` — all array elements (triggers leaf-scan pass)

No other operators (filter, recursive descent, etc.) are supported.

### 18.9 Schema Trie Memory Management

**Ceiling:** 100 MB allocated for the global Schema Trie.

**Trigger:** At 90 MB usage, a background **Scavenger Thread** is launched to reclaim the next 10 MB buffer before the ceiling is hit.

**Eviction policy (Leaf-Level):**

1. Identify leaf nodes where `observation_count < 5` (rare, likely one-off keys).
2. Identify internal nodes where `child_count < 2` (linear, non-branching paths that add depth without structural diversity).
3. **Merge to blob:** Evicted nodes are collapsed — their subtree is replaced with a single `<VAR_MAP>` token (for object subtrees) or `<OPAQUE_JSON>` token (for mixed/unknown subtrees). From that point forward, lumen stops tracking their internal schema and treats them as raw strings for the remainder of the run.

A warning is emitted to stderr when the scavenger fires: `[WARN] schema trie at 90MB — evicting long-tail nodes`.

### 18.10 Text Field Classification — Entropy Filter

The entropy filter runs after the length/word-count heuristic classifies a string as a potential `UnstructuredLine` or `UnstructuredMultiline`.

**Algorithm:**

1. **SIMD byte histogram**: maintain a 256-entry `u32` array on the stack. Use SIMD to process the string in 16- or 32-byte lanes, incrementing the count for each byte value. This is O(N) with no heap allocation.
2. **Opaque fast-exit**: if any byte outside the printable ASCII range (0x20–0x7E, plus `\t`, `\n`, `\r`) is encountered, immediately classify the string as `Scalar` (binary or encoded blob — not worth clustering).
3. **Approximate entropy**: compute Shannon entropy using a pre-built 256-entry lookup table of `p·log₂(p)` values (one table lookup per distinct byte value, not per byte). This keeps the entropy calculation O(alphabet\_size) = O(256), independent of string length.
4. **Classification**: if `entropy > 3.5 bits/byte` → `Scalar` (high-entropy = compressed/encoded data, e.g. JWT, base64, UUID). Otherwise the original `UnstructuredLine`/`UnstructuredMultiline` classification stands.

The entropy threshold is configurable via `--entropy-threshold <F>` (default `3.5`).

### 18.11 Cross-Path Template Identity

A Drain template is identified by its **token pattern hash alone**, independent of the JSON path it was extracted from. When the same template is observed at multiple paths:

- A **single CMS entry** tracks the combined frequency across all paths: `CMS(template_id)` is incremented regardless of origin path.
- The template's `source_paths` field (a `SmallVec<[Arc<str>; 2]>`) accumulates every distinct path that contributed records to it.
- The **Formatter** renders such templates as a **Multi-Path entity**, listing all contributing paths beneath the cluster header:

```
[×6,021] Failed to connect to * on port *
  paths: results[].message (×4,200)  errors[].description (×1,821)
  host: (52 distinct)  port: 3 distinct: [5432, 6379, 9200]
```

Novelty and anomaly scoring use the single merged CMS frequency, ensuring that a template seen across many paths is not incorrectly flagged as novel simply because one path is new.
