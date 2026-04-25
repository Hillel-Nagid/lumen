use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use lumen::cli::{Args, ModeOverride, OutputFormat};
use lumen::pipeline;

fn unique_temp_file(name: &str, ext: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.push(format!("lumen-{name}-{nonce}.{ext}"));
    path
}

fn test_args(input: PathBuf, output: PathBuf) -> Args {
    Args {
        file: Some(input),
        output: Some(output),
        format: OutputFormat::Text,
        tokens: None,
        bytes_per_token: 4.0,
        mode: ModeOverride::Log,
        project: None,
        threads: None,
        chunk_size: 16 * 1024 * 1024,
        memory_limit: 512,
        sim_threshold: 0.5,
        max_children: 128,
        depth: 4,
        min_cluster_size: 2,
        multiline_start: None,
        history_runs: 10,
        decay: 168.0,
        no_state: true,
        reset_state: false,
        retrain_dict: false,
        json_path: None,
        max_depth: 12,
        max_array_samples: 3,
        max_array_inline: 20,
        schema_only: false,
        entropy_threshold: 3.5,
        verbose: false,
        quiet: true,
    }
}

#[test]
fn a7_log_fixture_produces_non_empty_text_output() {
    let input_path = unique_temp_file("a7-input", "log");
    let output_path = unique_temp_file("a7-output", "txt");

    let fixture = "\
2026-04-25T20:11:00Z INFO Connected to db host=db-01 latency_ms=12
2026-04-25T20:11:01Z WARN Timeout contacting db host=db-02 latency_ms=450
2026-04-25T20:11:02Z INFO Connected to db host=db-01 latency_ms=10
";

    fs::write(&input_path, fixture).expect("write input fixture");

    let args = test_args(input_path.clone(), output_path.clone());
    let run_result = pipeline::run(args);

    // Cleanup temp files even if assertions fail.
    let _ = fs::remove_file(&input_path);

    assert!(run_result.is_ok(), "pipeline run failed: {run_result:?}");

    let output = fs::read_to_string(&output_path).expect("read output file");
    let _ = fs::remove_file(&output_path);

    assert!(
        !output.trim().is_empty(),
        "expected non-empty text output, got empty output"
    );
}
