use criterion::{criterion_group, criterion_main, Criterion, Throughput};

// ── Sample data ────────────────────────────────────────────────────────────────

const SAMPLE_LOGFMT: &[u8] = b"level=INFO msg=\"Connected to database\" host=localhost port=5432 latency_ms=12\n";
const SAMPLE_JSON:   &[u8] = b"{\"level\":\"INFO\",\"msg\":\"Connected to database\",\"host\":\"localhost\",\"port\":5432}\n";

// ── Mode detection ────────────────────────────────────────────────────────────

fn bench_probe_mode(c: &mut Criterion) {
    use lumen::ingest::detect;
    use lumen::cli::ModeOverride;

    let mut group = c.benchmark_group("probe_mode");

    let log_sample: Vec<u8> = SAMPLE_LOGFMT.repeat(200);
    let json_sample: Vec<u8> = SAMPLE_JSON.repeat(200);

    group.throughput(Throughput::Bytes(log_sample.len() as u64));
    group.bench_function("logfmt", |b| {
        b.iter(|| detect::probe_mode(std::hint::black_box(&log_sample), ModeOverride::Auto))
    });
    group.bench_function("ndjson", |b| {
        b.iter(|| detect::probe_mode(std::hint::black_box(&json_sample), ModeOverride::Auto))
    });

    group.finish();
}

// ── CMS ────────────────────────────────────────────────────────────────────────

fn bench_cms(c: &mut Criterion) {
    use lumen::scorer::cms::CountMinSketch;

    let mut group = c.benchmark_group("count_min_sketch");

    let mut cms = CountMinSketch::new();
    let ids: Vec<u64> = (0..1024).collect();

    group.bench_function("increment_1k_ids", |b| {
        b.iter(|| {
            for &id in &ids {
                cms.increment(std::hint::black_box(id), 1);
            }
        })
    });

    group.bench_function("estimate_1k_ids", |b| {
        b.iter(|| {
            for &id in &ids {
                let _ = cms.estimate(std::hint::black_box(id));
            }
        })
    });

    group.finish();
}

// ── Line iteration ────────────────────────────────────────────────────────────

fn bench_line_iter(c: &mut Criterion) {
    use lumen::ingest::LineIter;

    let data: Vec<u8> = SAMPLE_LOGFMT.repeat(100_000); // ~8 MB
    let mut group = c.benchmark_group("line_iter");
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("memchr_newline_scan", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for _line in LineIter::new(std::hint::black_box(&data)) {
                count += 1;
            }
            count
        })
    });

    group.finish();
}

criterion_group!(benches, bench_probe_mode, bench_cms, bench_line_iter);
criterion_main!(benches);
