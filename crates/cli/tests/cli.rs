#![cfg(windows)]
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustsearch"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(15) {
            child.kill().unwrap();
            panic!("CLI exceeded 15 second test deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}
fn json(args: &[&str]) -> Value {
    let output = run(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn real_cli_walk_search_benchmark_and_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("fixture");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("small.PDF"), b"abc").unwrap();
    std::fs::write(root.join("large.pdf"), [0u8; 1024]).unwrap();
    std::fs::write(root.join("İstanbul.txt"), []).unwrap();
    let snapshot = dir.path().join("index.rsidx");
    let report = dir.path().join("report.json");
    let result = json(&[
        "index",
        "--root",
        path(&root),
        "--sizes",
        "--output",
        path(&snapshot),
        "--report",
        path(&report),
    ]);
    assert_eq!(result["index"]["records"], 3);
    assert_eq!(result["index"]["unknown_size_files"], 0);
    assert!(report.exists());
    let result = json(&[
        "search",
        "--index",
        path(&snapshot),
        "--ext",
        "pdf",
        "--min-size",
        "1KiB",
        "--json",
    ]);
    assert_eq!(result["total"], 1);
    assert_eq!(result["cache"]["state"], "hit");
    assert_eq!(result["rows"][0]["name"], "large.pdf");
    assert_eq!(result["rows"][0]["logical_size"], 1024);
    let result = json(&[
        "search",
        "--index",
        path(&snapshot),
        "--largest",
        "--limit",
        "2",
        "--json",
    ]);
    assert_eq!(result["rows"][0]["name"], "large.pdf");
    assert_eq!(result["rows"][1]["name"], "small.PDF");
    assert_eq!(result["cache"]["state"], "hit");
    let cache_file = rustsearch_query::cache_path(&snapshot);
    let cache_bytes = std::fs::read(&cache_file).unwrap();
    let uncached = json(&[
        "search",
        "--index",
        path(&snapshot),
        "--largest",
        "--limit",
        "2",
        "--json",
        "--no-cache",
    ]);
    assert_eq!(uncached["rows"], result["rows"]);
    assert_eq!(uncached["cache"]["state"], "disabled");
    assert_eq!(std::fs::read(&cache_file).unwrap(), cache_bytes);
    let text = run(&["search", "--index", path(&snapshot), "--query", "large"]);
    assert!(text.status.success());
    assert!(String::from_utf8(text.stdout).unwrap().contains("prepare "));
    let result = json(&[
        "bench",
        "--index",
        path(&snapshot),
        "--iterations",
        "2",
        "--warmup",
        "0",
    ]);
    assert_eq!(result["cases"].as_array().unwrap().len(), 9);
    assert!(result["cases"][0]["p95_ms"].is_null());
    assert!(
        !run(&["refresh", "--index", path(&snapshot)])
            .status
            .success()
    );
    assert!(
        !run(&["info", "--index", path(&dir.path().join("missing.rsidx"))])
            .status
            .success()
    );
    assert!(
        !run(&["search", "--index", path(&snapshot), "--min-size", "1oops"])
            .status
            .success()
    );
    let original = std::fs::read(&snapshot).unwrap();
    assert!(
        !run(&[
            "bench",
            "--index",
            path(&snapshot),
            "--report",
            path(&snapshot)
        ])
        .status
        .success()
    );
    assert_eq!(std::fs::read(&snapshot).unwrap(), original);
    let compacted = dir.path().join("compacted.rsidx");
    let result = json(&[
        "compact",
        "--index",
        path(&snapshot),
        "--output",
        path(&compacted),
    ]);
    assert_eq!(result["index"]["records"], 3);
    assert_eq!(
        json(&[
            "search",
            "--index",
            path(&compacted),
            "--ext",
            "pdf",
            "--json"
        ])["total"],
        2
    );
    assert!(
        !run(&[
            "compact",
            "--index",
            path(&snapshot),
            "--output",
            path(&snapshot)
        ])
        .status
        .success()
    );
    assert_eq!(std::fs::read(&snapshot).unwrap(), original);
    std::fs::write(&snapshot, b"broken").unwrap();
    assert!(
        !run(&["search", "--index", path(&snapshot)])
            .status
            .success()
    );
}
