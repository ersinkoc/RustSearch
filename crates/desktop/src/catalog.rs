use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub snapshots: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            std::fs::metadata(path)?.len() <= 1024 * 1024,
            "catalog is too large"
        );
        let catalog: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(catalog.version == 1, "unsupported computer catalog version");
        ensure!(
            !catalog.snapshots.is_empty() && catalog.snapshots.len() <= 26,
            "catalog must contain 1–26 snapshots"
        );
        ensure!(
            catalog
                .snapshots
                .iter()
                .all(|p| p.is_absolute() && p.extension().is_some_and(|e| e == "rsidx")),
            "catalog requires absolute snapshot paths"
        );
        let mut unique = std::collections::HashSet::new();
        for snapshot in &catalog.snapshots {
            ensure!(
                unique.insert(
                    snapshot
                        .canonicalize()
                        .with_context(|| format!("open {}", snapshot.display()))?
                ),
                "catalog contains duplicate snapshots"
            );
        }
        Ok(catalog)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        rustsearch_storage::write_derived_atomic(path, |file| {
            serde_json::to_writer_pretty(file, self).context("save computer catalog")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_versions_relative_paths_and_duplicate_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer.rscatalog");
        let source = dir.path().join("source.rsidx");
        std::fs::write(&source, b"placeholder").unwrap();
        for catalog in [
            Catalog {
                version: 2,
                snapshots: vec![source.clone()],
                warnings: vec![],
            },
            Catalog {
                version: 1,
                snapshots: vec![PathBuf::from("relative.rsidx")],
                warnings: vec![],
            },
            Catalog {
                version: 1,
                snapshots: vec![source.clone(), source.clone()],
                warnings: vec![],
            },
            Catalog {
                version: 1,
                snapshots: vec![],
                warnings: vec![],
            },
        ] {
            catalog.save(&path).unwrap();
            assert!(Catalog::load(&path).is_err());
        }
        Catalog {
            version: 1,
            snapshots: vec![source.clone()],
            warnings: vec![],
        }
        .save(&path)
        .unwrap();
        assert_eq!(Catalog::load(&path).unwrap().snapshots, vec![source]);
    }
}
