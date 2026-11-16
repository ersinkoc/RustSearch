//! Compare every persisted logical record, including original UTF-16 spelling and file reference.
use anyhow::{Result, ensure};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 3, "verify_compaction <before> <after>");
    let a = rustsearch_storage::load(std::path::Path::new(&args[1]))?;
    let b = rustsearch_storage::load(std::path::Path::new(&args[2]))?;
    ensure!(
        a.updates.is_empty() && b.updates.is_empty(),
        "use consolidated snapshots"
    );
    ensure!(
        a.entries.len() == b.entries.len()
            && a.metadata.volume_id == b.metadata.volume_id
            && a.metadata.checkpoint == b.metadata.checkpoint,
        "snapshot scope differs"
    );
    for (old, new) in a.entries.iter().zip(&b.entries) {
        ensure!(
            (old.id, old.parent, a.usn(old), a.size(old), a.flags(old))
                == (new.id, new.parent, b.usn(new), b.size(new), b.flags(new)),
            "record metadata differs"
        );
        ensure!(
            a.name(old) == b.name(new) && a.key(old) == b.key(new),
            "name or search key differs"
        );
    }
    println!(
        "Verified {} logical records: IDs, parents, USNs, sizes, flags, UTF-16 names and search keys unchanged",
        a.entries.len()
    );
    Ok(())
}
