use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rustsearch_core::{DIRECTORY, Index, SIZE_KNOWN};
use rustsearch_query::{Kind, Mode, Query, SearchIndex};
use rustsearch_windows::{ProcessSample, process_sample};
use serde::Serialize;
use serde_json::{Value, json};
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "rustsearch",
    version,
    about = "Windows NTFS search feasibility CLI. Local metadata only; snapshots are not a live index."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect NTFS volume and existing journal; requires elevation.
    Probe {
        #[arg(long)]
        volume: String,
    },
    /// Build a snapshot. --volume uses NTFS USN enumeration; --root uses normal directory traversal.
    Index {
        #[arg(long, required_unless_present = "root", conflicts_with = "root")]
        volume: Option<String>,
        #[arg(long, required_unless_present = "volume", conflicts_with = "volume")]
        root: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        sizes: bool,
        /// Native backend only: explicitly limit file size probes; remaining sizes stay unknown.
        #[arg(long, requires = "sizes", conflicts_with = "root")]
        size_limit: Option<usize>,
        /// Skip the one-time persistent search cache preparation.
        #[arg(long)]
        no_cache: bool,
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Append a committed USN delta; the base snapshot remains unchanged.
    Refresh {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        sizes: bool,
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Search snapshot, returning full count and at most --limit rows. Does not rescan disk.
    Search {
        #[arg(long)]
        index: PathBuf,
        #[command(flatten)]
        filter: Filter,
        #[arg(long)]
        json: bool,
        #[arg(long, value_enum, default_value = "indexed")]
        strategy: Strategy,
        /// Rebuild in memory without reading or writing the persistent search cache.
        #[arg(long)]
        no_cache: bool,
    },
    /// Warm in-process full-count queries; snapshot load is measured separately.
    Bench {
        #[arg(long)]
        index: PathBuf,
        #[arg(long,default_value_t=1000,value_parser=clap::value_parser!(u32).range(1..=100_000))]
        iterations: u32,
        #[arg(long,default_value_t=10,value_parser=clap::value_parser!(u32).range(0..=1000))]
        warmup: u32,
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "indexed")]
        strategy: Strategy,
        #[arg(long)]
        verify_linear: bool,
        #[arg(long)]
        no_cache: bool,
    },
    /// Consolidate deltas and deduplicate names into a NEW standalone snapshot.
    Compact {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long)]
        no_cache: bool,
    },
    /// Measure an already loaded, inactive index. This is not a journal-watching daemon.
    Idle {
        #[arg(long)]
        index: PathBuf,
        #[arg(long,default_value_t=10,value_parser=clap::value_parser!(u32).range(1..=600))]
        seconds: u32,
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long)]
        no_cache: bool,
    },
    /// Snapshot scope, counts and allocated arena memory; never prints file names.
    Info {
        #[arg(long)]
        index: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum MatchMode {
    Contains,
    Prefix,
    Exact,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Strategy {
    Indexed,
    Linear,
}
#[derive(Clone, Copy, ValueEnum)]
enum EntryKind {
    Any,
    File,
    Folder,
}
#[derive(Args)]
struct Filter {
    #[arg(long, default_value = "")]
    query: String,
    #[arg(long, value_enum, default_value = "contains")]
    mode: MatchMode,
    #[arg(long, value_enum, default_value = "any")]
    kind: EntryKind,
    #[arg(long)]
    ext: Option<String>,
    #[arg(long,value_parser=parse_size)]
    min_size: Option<u64>,
    #[arg(long)]
    largest: bool,
    #[arg(long,default_value_t=100,value_parser=clap::value_parser!(u32).range(0..=1_000_000))]
    limit: u32,
}
impl Filter {
    fn query(self) -> Query {
        Query {
            text: self.query,
            mode: match self.mode {
                MatchMode::Contains => Mode::Contains,
                MatchMode::Prefix => Mode::Prefix,
                MatchMode::Exact => Mode::Exact,
            },
            kind: match self.kind {
                EntryKind::Any => Kind::Any,
                EntryKind::File => Kind::File,
                EntryKind::Folder => Kind::Folder,
            },
            extension: self.ext,
            min_size: self.min_size,
            largest: self.largest,
            limit: self.limit as usize,
        }
    }
}

fn parse_size(value: &str) -> std::result::Result<u64, String> {
    let boundary = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let number = value[..boundary].parse::<u64>().map_err(|_| {
        "size must be a nonnegative integer with optional B/KiB/MiB/GiB/TiB suffix".to_owned()
    })?;
    let multiplier = match value[boundary..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1024,
        "mib" => 1024u64.pow(2),
        "gib" => 1024u64.pow(3),
        "tib" => 1024u64.pow(4),
        "kb" => 1000,
        "mb" => 1000u64.pow(2),
        "gb" => 1000u64.pow(3),
        "tb" => 1000u64.pow(4),
        _ => return Err("unknown size suffix".into()),
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| "size overflow".into())
}

fn emit(value: &impl Serialize, path: Option<&Path>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if let Some(path) = path {
        std::fs::write(path, &bytes).context("write report")?;
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn different_paths(report: &Path, snapshot: &Path) -> Result<()> {
    let resolve = |p: &Path| -> Result<PathBuf> {
        let full = std::path::absolute(p)?;
        Ok(full
            .parent()
            .context("path has no parent")?
            .canonicalize()?
            .join(full.file_name().context("path has no filename")?))
    };
    ensure!(
        !resolve(report)?
            .to_string_lossy()
            .eq_ignore_ascii_case(&resolve(snapshot)?.to_string_lossy()),
        "report must not overwrite the snapshot"
    );
    Ok(())
}
fn check_report(report: Option<&Path>, snapshot: &Path) -> Result<()> {
    if let Some(report) = report {
        different_paths(report, snapshot)?;
        ensure!(
            !report
                .extension()
                .is_some_and(|e| ["rsidx", "delta", "lock", "search"]
                    .iter()
                    .any(|ext| e.eq_ignore_ascii_case(ext))),
            "report must not use an index/sidecar extension"
        );
    }
    Ok(())
}

fn summary(index: &Index) -> Value {
    let (records, folders, sized, reparse) = index.counts();
    json!({"backend":index.metadata.source,"filesystem":index.metadata.filesystem,"records":records,"files":records-folders,"folders":folders,"sized_files":sized,"unknown_size_files":records-folders-sized,"skipped_entries":index.metadata.skipped_entries,"reparse_points_not_followed":reparse,"pending_refresh":index.metadata.pending_refresh.len(),"checkpoint":index.metadata.checkpoint,"log_sequence":index.log_sequence,"overlay_entries":index.updates.len(),"created_unix_ms":index.metadata.created_unix_ms,"arena_allocated_bytes":index.allocated_bytes(),"logical_payload_bytes":index.entries.len()*48+index.names.len()*2+index.keys.len(),"name_coverage":if index.metadata.source=="ntfs-usn" {"one canonical name per file reference; additional hard-link names not enumerated"} else {"reachable directory entries; no reparse traversal; synthetic IDs"},"freshness":"point-in-time observations; use refresh/rebuild before relying on current filesystem state","size_semantics":"default stream logical length only; not allocated disk space or reclaimable bytes","search_semantics":"Unicode lowercase substring/prefix/exact on names; no locale collation or Unicode normalization"})
}

fn resources(before: &ProcessSample, start: Instant) -> Result<Value> {
    let after = process_sample()?;
    let wall_ms = start.elapsed().as_secs_f64() * 1000.;
    let cpu = after.cpu_ms - before.cpu_ms;
    Ok(json!({
        "wall_ms":wall_ms,
        "cpu_ms":cpu,
        "cpu_percent_one_core":if wall_ms>0. {cpu/wall_ms*100.} else {0.},
        "private_bytes":after.private_bytes,
        "peak_private_bytes_process_lifetime":after.peak_private_bytes,
        "working_set_bytes":after.working_set_bytes,
        "peak_working_set_bytes_process_lifetime":after.peak_working_set_bytes,
        "read_transfer_bytes":after.read_transfer_bytes.saturating_sub(before.read_transfer_bytes),
        "write_transfer_bytes":after.write_transfer_bytes.saturating_sub(before.write_transfer_bytes),
        "other_transfer_bytes":after.other_transfer_bytes.saturating_sub(before.other_transfer_bytes),
        "io_semantics":"process transfer counters including IOCTL/other, not physical disk I/O; report write/stdout excluded"
    }))
}

fn bench_cases() -> Vec<(&'static str, Query)> {
    vec![
        (
            "rare_contains",
            Query {
                text: "rustsearch".into(),
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "common_contains",
            Query {
                text: "a".into(),
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "no_match",
            Query {
                text: "__rustsearch_nonexistent_924efa__".into(),
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "prefix",
            Query {
                text: "readme".into(),
                mode: Mode::Prefix,
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "exact",
            Query {
                text: "cargo.toml".into(),
                mode: Mode::Exact,
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "extension",
            Query {
                extension: Some("rs".into()),
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "large_files",
            Query {
                min_size: Some(1024u64.pow(3)),
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "largest_100",
            Query {
                largest: true,
                limit: 100,
                ..Default::default()
            },
        ),
        (
            "all",
            Query {
                limit: 100,
                ..Default::default()
            },
        ),
    ]
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let before = process_sample()?;
    let start = Instant::now();
    match cli.command {
        Command::Probe { volume } => {
            let (info, journal) = rustsearch_windows::probe(&volume)?;
            emit(&json!({"volume":info,"journal":journal}), None)?;
        }
        Command::Index {
            volume,
            root,
            output,
            sizes,
            size_limit,
            no_cache,
            report,
        } => {
            check_report(report.as_deref(), &output)?;
            let (mut index, stats) = if let Some(volume) = volume {
                rustsearch_windows::scan_ntfs(
                    &volume,
                    if sizes {
                        Some(size_limit.unwrap_or(usize::MAX))
                    } else {
                        None
                    },
                )?
            } else {
                rustsearch_windows::walk(root.as_deref().context("missing root")?, sizes)?
            };
            if !index.updates.is_empty() {
                index.consolidate()?;
            }
            let save = Instant::now();
            index.snapshot_identity = Some(rustsearch_storage::save(&output, &index)?);
            let save_ms = save.elapsed().as_secs_f64() * 1000.;
            let cache = (!no_cache).then(|| SearchIndex::prepare(&index, &output, true).1);
            emit(
                &json!({"command":"index","cache":cache,"version":env!("CARGO_PKG_VERSION"),"index":summary(&index),"phases":stats,"save_ms":save_ms,"snapshot_bytes":std::fs::metadata(&output)?.len(),"resources":resources(&before,start)?}),
                report.as_deref(),
            )?;
        }
        Command::Refresh {
            index,
            sizes,
            report,
        } => {
            check_report(report.as_deref(), &index)?;
            let guard = rustsearch_storage::WriteGuard::acquire(&index)?;
            let mut source = rustsearch_storage::load(&index)?;
            let (delta, mut stats) = rustsearch_windows::refresh(&source, sizes)?;
            let save = Instant::now();
            let append = rustsearch_storage::append_delta(&guard, &source, &delta)?;
            source.apply_delta(delta)?;
            let save_ms = save.elapsed().as_secs_f64() * 1000.;
            let (records, folders, sized, _) = source.counts();
            stats.records = records as u64;
            stats.folders = folders as u64;
            stats.files = (records - folders) as u64;
            stats.sized_files = sized as u64;
            emit(
                &json!({"command":"refresh","index":summary(&source),"phases":stats,"save_ms":save_ms,"append":append,"resources":resources(&before,start)?}),
                report.as_deref(),
            )?;
        }
        Command::Compact {
            index,
            output,
            report,
            no_cache,
        } => {
            ensure!(!output.exists(), "compaction output must be a new path");
            different_paths(&output, &index)?;
            check_report(report.as_deref(), &output)?;
            check_report(report.as_deref(), &index)?;
            let mut source = rustsearch_storage::load(&index)?;
            let compact = Instant::now();
            source.consolidate()?;
            let compact_ms = compact.elapsed().as_secs_f64() * 1000.;
            source.snapshot_identity = Some(rustsearch_storage::save(&output, &source)?);
            let cache = (!no_cache).then(|| SearchIndex::prepare(&source, &output, true).1);
            emit(
                &json!({"command":"compact","cache":cache,"index":summary(&source),"compact_ms":compact_ms,"snapshot_bytes":std::fs::metadata(&output)?.len(),"resources":resources(&before,start)?}),
                report.as_deref(),
            )?;
        }
        Command::Search {
            index,
            filter,
            json: as_json,
            strategy,
            no_cache,
        } => {
            let snapshot = index;
            let index = rustsearch_storage::load(&snapshot)?;
            let load_ms = start.elapsed().as_secs_f64() * 1000.;
            let query = filter.query();
            let prepare = Instant::now();
            let (prepared, cache) = if matches!(strategy, Strategy::Indexed) {
                let (p, c) = SearchIndex::prepare(&index, &snapshot, !no_cache);
                (Some(p), Some(c))
            } else {
                (None, None)
            };
            let prepare_ms = prepare.elapsed().as_secs_f64() * 1000.;
            let begin = Instant::now();
            let matches = prepared
                .as_ref()
                .map(|p| p.execute(&query))
                .unwrap_or_else(|| query.execute(&index));
            let query_ms = begin.elapsed().as_secs_f64() * 1000.;
            let rows:Vec<Value>=matches.rows.iter().map(|&row| {
                let entry=index.get(row).expect("query returned live ID");let path=index.path_utf16(entry);
                json!({"id":entry.id,"name":String::from_utf16_lossy(&index.name(entry)),"path":path.as_ref().ok().map(|p|String::from_utf16_lossy(p)),"path_error":path.as_ref().err().map(ToString::to_string),"display_is_lossy":String::from_utf16(&index.name(entry)).is_err(),"pending_refresh":index.metadata.pending_refresh.binary_search(&entry.id).is_ok(),"kind":if entry.flags&DIRECTORY!=0 {"folder"} else {"file"},"logical_size":if entry.flags&SIZE_KNOWN!=0 {Some(index.size(entry))} else {None}})
            }).collect();
            if as_json {
                emit(
                    &json!({"total":matches.total,"returned":rows.len(),"load_ms":load_ms,"prepare_ms":prepare_ms,"query_ms":query_ms,"cache":cache,"search_index_bytes":prepared.as_ref().map(|p|p.allocated_bytes()).unwrap_or(0),"timing_semantics":"full count + bounded results; excludes load, search index preparation and path hydration","index":summary(&index),"rows":rows,"resources":resources(&before,start)?}),
                    None,
                )?;
            } else {
                let mut stdout = io::stdout().lock();
                for row in rows {
                    writeln!(
                        stdout,
                        "{}\t{}",
                        row["logical_size"]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        row["path"].as_str().unwrap_or("<unresolved path>")
                    )?;
                }
                writeln!(
                    stdout,
                    "{} matches; query {:.3} ms; load {:.3} ms; prepare {:.3} ms; cache {}; snapshot, not live",
                    matches.total,
                    query_ms,
                    load_ms,
                    prepare_ms,
                    cache.as_ref().map(|c| c.state).unwrap_or("linear")
                )?;
            }
        }
        Command::Bench {
            index,
            iterations,
            warmup,
            report,
            strategy,
            verify_linear,
            no_cache,
        } => {
            check_report(report.as_deref(), &index)?;
            let snapshot = index;
            let index = rustsearch_storage::load(&snapshot)?;
            let load_ms = start.elapsed().as_secs_f64() * 1000.;
            let prepare = Instant::now();
            let (prepared, cache) = if matches!(strategy, Strategy::Indexed) {
                let (p, c) = SearchIndex::prepare(&index, &snapshot, !no_cache);
                (Some(p), Some(c))
            } else {
                (None, None)
            };
            let prepare_ms = prepare.elapsed().as_secs_f64() * 1000.;
            let mut cases = Vec::new();
            for (name, query) in bench_cases() {
                if verify_linear {
                    ensure!(
                        prepared
                            .as_ref()
                            .map(|p| p.execute(&query))
                            .unwrap_or_else(|| query.execute(&index))
                            == query.execute(&index),
                        "indexed result mismatch in {name}"
                    );
                }
                let execute = || {
                    prepared
                        .as_ref()
                        .map(|p| p.execute(black_box(&query)))
                        .unwrap_or_else(|| query.execute(black_box(&index)))
                };
                for _ in 0..warmup {
                    black_box(execute());
                }
                let mut samples = Vec::with_capacity(iterations as usize);
                let mut total = 0;
                let mut returned = 0;
                for _ in 0..iterations {
                    let begin = Instant::now();
                    let result = black_box(execute());
                    samples.push(begin.elapsed().as_secs_f64() * 1000.);
                    total = result.total;
                    returned = result.rows.len();
                }
                samples.sort_by(f64::total_cmp);
                let percentile = |p: f64| {
                    samples[((samples.len() as f64 * p).ceil() as usize).saturating_sub(1)]
                };
                cases.push(json!({"case":name,"samples":iterations,"warmup":warmup,"total_matches":total,"returned":returned,"min_ms":samples[0],"mean_ms":samples.iter().sum::<f64>()/samples.len() as f64,"p50_ms":percentile(0.5),"p95_ms":if iterations>=100 {Some(percentile(0.95))} else {None},"p99_ms":if iterations>=1000 {Some(percentile(0.99))} else {None},"max_ms":samples[samples.len()-1]}));
                eprintln!("benchmark {name}: {} samples complete", iterations);
            }
            emit(
                &json!({"command":"bench","version":env!("CARGO_PKG_VERSION"),"strategy":format!("{strategy:?}"),"cache":cache,"verified_linear":verify_linear,"load_ms":load_ms,"prepare_ms":prepare_ms,"search_index_bytes":prepared.as_ref().map(|p|p.allocated_bytes()).unwrap_or(0),"distinct_name_groups":prepared.as_ref().map(|p|p.distinct_groups()),"index":summary(&index),"measurement":"warm in-process name queries, full count + top/first 100; no preparation, path hydration or UI; result allocation included","cache_state":"OS cache uncontrolled; not a cold-disk benchmark","cases":cases,"resources":resources(&before,start)?}),
                report.as_deref(),
            )?;
        }
        Command::Idle {
            index,
            seconds,
            report,
            no_cache,
        } => {
            check_report(report.as_deref(), &index)?;
            let snapshot = index;
            let index = rustsearch_storage::load(&snapshot)?;
            let (prepared, cache) = SearchIndex::prepare(&index, &snapshot, !no_cache);
            let summary = summary(&index);
            let idle_before = process_sample()?;
            let idle_start = Instant::now();
            std::thread::sleep(Duration::from_secs(seconds as u64));
            black_box(&index);
            black_box(&prepared);
            emit(
                &json!({"command":"idle","cache":cache,"index":summary,"measurement":"loaded inactive CLI, no watcher/service/UI; not evidence of live journal daemon idle cost","resources":resources(&idle_before,idle_start)?}),
                report.as_deref(),
            )?;
        }
        Command::Info { index } => {
            let index = rustsearch_storage::load(&index)?;
            emit(
                &json!({"index":summary(&index),"resources":resources(&before,start)?}),
                None,
            )?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
        {
            return;
        }
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizes_have_explicit_units_and_reject_overflow_or_unknown_suffix() {
        assert_eq!(parse_size("1GiB").unwrap(), 1_073_741_824);
        assert_eq!(parse_size("1GB").unwrap(), 1_000_000_000);
        assert_eq!(parse_size("0").unwrap(), 0);
        for value in ["-1", "1.5GiB", "1oops", "18446744073709551615GiB", ""] {
            assert!(parse_size(value).is_err());
        }
    }
}
