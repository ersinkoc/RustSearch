//! Controlled update benchmark on one frozen real-disk snapshot. Run each mode in a fresh process.
//! The 128 metadata edits are synthetic; live NTFS correctness is a separate acceptance script.
use anyhow::{Result, ensure};
use rustsearch_core::{DIRECTORY, Delta, Record};
use rustsearch_windows::process_sample;
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 5,
        "update_compare <rewrite|delta> <source> <new-output> <report>"
    );
    let mode = &args[1];
    ensure!(matches!(mode.as_str(), "rewrite" | "delta"), "unknown mode");
    let source_path = PathBuf::from(&args[2]);
    let output = PathBuf::from(&args[3]);
    let report = PathBuf::from(&args[4]);
    ensure!(
        !output.exists() && !report.exists(),
        "benchmark outputs must be new paths"
    );
    let mut source = rustsearch_storage::load(&source_path)?;
    ensure!(
        source.updates.is_empty(),
        "benchmark needs a consolidated snapshot"
    );
    let mut changes = BTreeMap::new();
    for e in source
        .entries
        .iter()
        .filter(|e| e.flags & DIRECTORY == 0)
        .take(128)
    {
        changes.insert(
            e.id,
            Some(Record {
                id: e.id,
                parent: e.parent,
                usn: source.usn(e),
                size: Some(source.size(e).saturating_add(7)),
                flags: source.flags(e),
                name: source.name(e).to_vec(),
            }),
        );
    }
    let metadata = source.metadata.clone();
    if mode == "delta" {
        // Same-volume hard link preserves the source ACL, avoids copying a gigabyte for setup,
        // and is safe here because append_delta never modifies the base inode.
        std::fs::hard_link(&source_path, &output)?;
    }
    let before = process_sample()?;
    let start = Instant::now();
    let writes;
    if mode == "rewrite" {
        let updated = source.apply(metadata, changes)?;
        drop(source);
        source = updated;
        rustsearch_storage::save(&output, &source)?;
        writes = std::fs::metadata(&output)?.len();
    } else {
        let delta = Delta {
            previous_sequence: source.log_sequence,
            previous_checkpoint: source.metadata.checkpoint.clone(),
            metadata,
            records: changes,
        };
        let guard = rustsearch_storage::WriteGuard::acquire(&output)?;
        writes = rustsearch_storage::append_delta(&guard, &source, &delta)?.bytes_appended;
        source.apply_delta(delta)?;
    }
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    let after = process_sample()?;
    let mut signature = 0u64;
    for e in source
        .entries
        .iter()
        .filter(|e| e.flags & DIRECTORY == 0)
        .take(128)
    {
        let live = source.get(e.id).unwrap();
        signature = signature.wrapping_mul(0x100000001b3) ^ live.id ^ source.size(live);
    }
    let result = json!({"mode":mode,"scope":"128 identical synthetic metadata edits on a frozen real NTFS snapshot; no filesystem file contents modified","changed_records":128,"result_signature":signature,"wall_update_ms":elapsed_ms,"cpu_update_ms":after.cpu_ms-before.cpu_ms,"private_bytes":after.private_bytes,"peak_private_bytes_process_lifetime":after.peak_private_bytes,"working_set_bytes":after.working_set_bytes,"bytes_persisted":writes,"process_write_transfer_bytes":after.write_transfer_bytes-before.write_transfer_bytes,"setup":"source load and hard-link creation excluded; each mode runs in a separate process"});
    std::fs::write(report, serde_json::to_vec_pretty(&result)?)?;
    println!("{result}");
    Ok(())
}
