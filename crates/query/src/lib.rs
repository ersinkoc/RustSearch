use rustsearch_core::{DIRECTORY, Index, SIZE_KNOWN};
use std::collections::BinaryHeap;
mod prepared;
pub use prepared::{CacheStats, SearchIndex, SearchTables, cache_path};

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    #[default]
    Contains,
    Prefix,
    Exact,
}
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    #[default]
    Any,
    File,
    Folder,
}
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: String,
    pub mode: Mode,
    pub kind: Kind,
    pub extension: Option<String>,
    pub min_size: Option<u64>,
    pub largest: bool,
    pub limit: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Matches {
    pub total: usize,
    pub rows: Vec<u64>,
}

impl Query {
    /// Full count plus at most limit results. No path hydration or disk I/O in the timed loop.
    pub fn execute(&self, index: &Index) -> Matches {
        let key = self.text.to_lowercase().into_bytes();
        let finder = memchr::memmem::Finder::new(&key);
        let extension = self
            .extension
            .as_ref()
            .map(|e| format!(".{}", e.trim_start_matches('.').to_lowercase()).into_bytes());
        let mut result = Matches {
            total: 0,
            rows: Vec::with_capacity(self.limit.min(index.entries.len())),
        };
        let mut top = BinaryHeap::new();
        for entry in index.live_entries() {
            let directory = entry.flags & DIRECTORY != 0;
            if matches!(self.kind, Kind::File) && directory
                || matches!(self.kind, Kind::Folder) && !directory
            {
                continue;
            }
            if let Some(size) = self.min_size
                && (directory || entry.flags & SIZE_KNOWN == 0 || index.size(entry) < size)
            {
                continue;
            }
            if self.largest && (directory || entry.flags & SIZE_KNOWN == 0) {
                continue;
            }
            let name = index.key(entry);
            if let Some(ext) = &extension
                && (directory || !name.ends_with(ext))
            {
                continue;
            }
            let matched = match self.mode {
                Mode::Contains => finder.find(name).is_some(),
                Mode::Prefix => name.starts_with(&key),
                Mode::Exact => name == key,
            };
            if !matched {
                continue;
            }
            result.total += 1;
            if self.largest {
                if self.limit == 0 {
                    continue;
                }
                // Reverse size makes the smallest retained file the heap root. ID breaks ties deterministically.
                let candidate = (std::cmp::Reverse(index.size(entry)), entry.id);
                if top.len() < self.limit {
                    top.push(candidate);
                } else if top.peek().is_some_and(|worst| candidate < *worst) {
                    top.pop();
                    top.push(candidate);
                }
            } else if result.rows.len() < self.limit {
                let position = result.rows.partition_point(|id| *id < entry.id);
                result.rows.insert(position, entry.id);
            } else if !self.largest
                && self.limit > 0
                && result.rows.last().is_some_and(|id| entry.id < *id)
            {
                let position = result.rows.partition_point(|id| *id < entry.id);
                result.rows.insert(position, entry.id);
                result.rows.pop();
            }
        }
        if self.largest {
            result.rows = top
                .into_sorted_vec()
                .into_iter()
                .map(|(_, id)| id)
                .collect();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustsearch_core::{Metadata, Record};
    fn fixture() -> Index {
        let mut i = Index::new(Metadata {
            source: "walk".into(),
            root: "D:\\test".into(),
            volume_id: "test".into(),
            filesystem: "NTFS".into(),
            root_id: 1,
            created_unix_ms: 0,
            checkpoint: None,
            pending_refresh: vec![],
            skipped_entries: 0,
            reparse_points_not_followed: 0,
        });
        for (id, name, size, flags) in [
            (2, "Report.PDF", Some(10), 0),
            (3, "report.pdf.old", Some(40), 0),
            (4, "İstanbul.txt", None, 0),
            (5, "report.pdf", None, DIRECTORY),
            (6, "large.PDF", Some(100), 0),
            (7, "tie.pdf", Some(100), 0),
        ] {
            i.push(Record {
                id,
                parent: 1,
                usn: 0,
                size,
                flags,
                name: name.encode_utf16().collect(),
            })
            .unwrap();
        }
        i.finish().unwrap();
        i
    }
    #[test]
    fn child_row_ranges_preserve_case_shadowing_and_moved_or_inserted_overlays() {
        let mut index = fixture();
        for (id, parent, name, flags) in [
            (1000, 1, "shared", DIRECTORY),
            (1001, 1000, "Report.PDF", 0),
            (1002, 1000, "common.txt", 0),
            (1003, 5, "common.txt", 0),
            (1004, 1000, "case.txt", 0),
        ] {
            index
                .push(Record {
                    id,
                    parent,
                    usn: 0,
                    size: None,
                    flags,
                    name: name.encode_utf16().collect(),
                })
                .unwrap();
        }
        index.finish().unwrap();
        for generation in 0..2 {
            let search = SearchIndex::build(&index);
            for parent in [1, 5, 1000, 2001, 9999] {
                let rows: Vec<_> = index
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.parent == parent)
                    .map(|(row, _)| (row as u32, row as u32 + 1))
                    .collect();
                for name in [
                    "Report.PDF",
                    "REPORT.PDF",
                    "report.pdf",
                    "common.txt",
                    "COMMON.TXT",
                    "missing",
                    "inserted.txt",
                    "moved.txt",
                ] {
                    let name: Vec<_> = name.encode_utf16().collect();
                    let overlay: Vec<_> = index
                        .updates
                        .values()
                        .filter_map(Option::as_ref)
                        .filter(|e| e.parent == parent)
                        .collect();
                    assert_eq!(
                        search.child(parent, &name).map(|e| e.id),
                        search
                            .child_in_ranges_and_overlay(
                                parent,
                                &name,
                                rows.iter().copied(),
                                &overlay
                            )
                            .map(|e| e.id)
                    );
                    assert_eq!(
                        search.child(parent, &name).map(|e| e.id),
                        search
                            .child_in_ranges(parent, &name, rows.iter().copied())
                            .map(|e| e.id)
                    );
                }
            }
            drop(search);
            if generation == 0 {
                let make = |id, parent, name: &str, flags| {
                    Some(Record {
                        id,
                        parent,
                        usn: 0,
                        size: None,
                        flags,
                        name: name.encode_utf16().collect(),
                    })
                };
                index
                    .apply_delta(rustsearch_core::Delta {
                        previous_sequence: index.log_sequence,
                        previous_checkpoint: None,
                        metadata: index.metadata.clone(),
                        records: std::collections::BTreeMap::from([
                            (2, None),
                            (1001, make(1001, 5, "moved.txt", 0)),
                            (2000, make(2000, 1000, "inserted.txt", 0)),
                            (2001, make(2001, 1, "fresh", DIRECTORY)),
                            (2002, make(2002, 2001, "inserted.txt", 0)),
                        ]),
                    })
                    .unwrap();
            }
        }
    }

    #[test]
    fn detached_tables_rebind_live_deltas_and_reject_another_base() {
        let mut index = fixture();
        index.snapshot_identity = Some([7; 24]);
        let search = SearchIndex::build(&index);
        assert_eq!(
            search
                .child(1, &"Report.PDF".encode_utf16().collect::<Vec<_>>())
                .unwrap()
                .id,
            2
        );
        let memory = search.allocated_bytes();
        let tables = search.detach();
        index
            .apply_delta(rustsearch_core::Delta {
                previous_sequence: 0,
                previous_checkpoint: None,
                metadata: index.metadata.clone(),
                records: std::collections::BTreeMap::from([
                    (
                        2,
                        Some(Record {
                            id: 2,
                            parent: 1,
                            usn: 1,
                            size: Some(999),
                            flags: 0,
                            name: "Renamed.txt".encode_utf16().collect(),
                        }),
                    ),
                    (3, None),
                ]),
            })
            .unwrap();
        let search = tables.attach(&index).unwrap();
        assert!(
            search
                .child(1, &"Report.PDF".encode_utf16().collect::<Vec<_>>())
                .is_some_and(|e| e.id == 5)
        );
        assert_eq!(
            search
                .child(1, &"Renamed.txt".encode_utf16().collect::<Vec<_>>())
                .unwrap()
                .id,
            2
        );
        for query in [
            Query {
                text: "report".into(),
                limit: 100,
                ..Default::default()
            },
            Query {
                largest: true,
                limit: 100,
                ..Default::default()
            },
            Query {
                mode: Mode::Exact,
                text: "Renamed.txt".into(),
                limit: 100,
                ..Default::default()
            },
        ] {
            assert_eq!(search.execute(&query), query.execute(&index));
        }
        assert!(search.allocated_bytes() < memory + 4096);
        let tables = search.detach();
        let mut different = fixture();
        different.snapshot_identity = Some([8; 24]);
        assert!(tables.attach(&different).is_err());
    }
    #[test]
    fn combines_filters_and_distinguishes_zero_from_unknown_size() {
        let i = fixture();
        let q = Query {
            text: "REPORT".into(),
            extension: Some("pdf".into()),
            kind: Kind::File,
            min_size: Some(0),
            limit: 100,
            ..Default::default()
        };
        let r = q.execute(&i);
        assert_eq!(r.total, 1);
        assert_eq!(r.rows[0], 2);
        let q = Query {
            min_size: Some(0),
            limit: 0,
            ..Default::default()
        };
        let r = q.execute(&i);
        assert_eq!(r.total, 4);
        assert!(r.rows.is_empty());
    }
    #[test]
    fn exact_prefix_unicode_and_empty_queries() {
        let i = fixture();
        let q = Query {
            text: "Report.PDF".into(),
            mode: Mode::Exact,
            limit: 100,
            ..Default::default()
        };
        assert_eq!(q.execute(&i).total, 2);
        let q = Query {
            text: "i\u{307}STAN".into(),
            mode: Mode::Prefix,
            limit: 100,
            ..Default::default()
        };
        assert_eq!(q.execute(&i).total, 1);
        assert_eq!(
            Query {
                limit: 1,
                ..Default::default()
            }
            .execute(&i)
            .total,
            6
        );
    }
    #[test]
    fn largest_excludes_unknowns_and_orders_ties_by_id() {
        let i = fixture();
        let q = Query {
            largest: true,
            limit: 3,
            ..Default::default()
        };
        let r = q.execute(&i);
        assert_eq!(r.total, 4);
        assert_eq!(r.rows, vec![6, 7, 3]);
    }

    #[test]
    fn single_ascii_queries_match_linear_across_filters_and_delta_generations() {
        use rustsearch_core::Delta;
        let mut index = fixture();
        for id in 100..300 {
            index
                .push(Record {
                    id,
                    parent: 1,
                    usn: 0,
                    size: (id % 3 != 0).then_some(id),
                    flags: if id % 7 == 0 { DIRECTORY } else { 0 },
                    name: format!("Az09-{:02}-🙂-Ω.pdf", id % 13)
                        .encode_utf16()
                        .collect(),
                })
                .unwrap();
        }
        index.finish().unwrap();
        for generation in 0..3 {
            if generation > 0 {
                let records = if generation == 1 {
                    std::collections::BTreeMap::from([
                        (2, None),
                        (
                            100,
                            Some(Record {
                                id: 100,
                                parent: 1,
                                usn: 1,
                                size: Some(0),
                                flags: DIRECTORY,
                                name: "no-match-folder".encode_utf16().collect(),
                            }),
                        ),
                        (
                            500,
                            Some(Record {
                                id: 500,
                                parent: 1,
                                usn: 1,
                                size: Some(99),
                                flags: 0,
                                name: "AZ09-created.pdf".encode_utf16().collect(),
                            }),
                        ),
                    ])
                } else {
                    std::collections::BTreeMap::from([
                        (500, None),
                        (
                            100,
                            Some(Record {
                                id: 100,
                                parent: 1,
                                usn: 2,
                                size: None,
                                flags: 0,
                                name: "AZ09-renamed.txt".encode_utf16().collect(),
                            }),
                        ),
                    ])
                };
                index
                    .apply_delta(Delta {
                        previous_sequence: index.log_sequence,
                        previous_checkpoint: index.metadata.checkpoint.clone(),
                        metadata: index.metadata.clone(),
                        records,
                    })
                    .unwrap();
            }
            let prepared = SearchIndex::build(&index);
            // Punctuation and Unicode control cases must still use real name matching.
            for byte in b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-" {
                for kind in [Kind::Any, Kind::File, Kind::Folder] {
                    for limit in [0, 1, 17, 1000] {
                        for min_size in [None, Some(0), Some(150)] {
                            for extension in [None, Some("pdf")] {
                                for largest in [false, true] {
                                    let query = Query {
                                        text: (*byte as char).to_string(),
                                        mode: Mode::Contains,
                                        kind,
                                        limit,
                                        min_size,
                                        extension: extension.map(str::to_string),
                                        largest,
                                    };
                                    assert_eq!(
                                        prepared.execute(&query),
                                        query.execute(&index),
                                        "generation={generation}, {query:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn prepared_queries_equal_linear_for_duplicates_unicode_filters_and_deltas() {
        use rustsearch_core::Delta;
        let mut index = fixture();
        for id in 8..308 {
            let name = [
                "foo.tar.gz",
                "Report.PDF",
                "İstanbul.txt",
                "🙂alpha",
                "nodot",
                "trailing.",
            ][(id as usize) % 6];
            index
                .push(Record {
                    id,
                    parent: 1,
                    usn: 0,
                    size: if id % 5 == 0 { None } else { Some(id % 17) },
                    flags: if id % 13 == 0 { DIRECTORY } else { 0 },
                    name: name.encode_utf16().collect(),
                })
                .unwrap();
        }
        index.finish().unwrap();
        let mut records = std::collections::BTreeMap::new();
        records.insert(2, None);
        records.insert(
            9,
            Some(Record {
                id: 9,
                parent: 1,
                usn: 1,
                size: Some(99),
                flags: 0,
                name: "Report.PDF".encode_utf16().collect(),
            }),
        );
        records.insert(
            400,
            Some(Record {
                id: 400,
                parent: 1,
                usn: 1,
                size: Some(0),
                flags: 0,
                name: "created.txt".encode_utf16().collect(),
            }),
        );
        index
            .apply_delta(Delta {
                previous_sequence: 0,
                previous_checkpoint: None,
                metadata: index.metadata.clone(),
                records,
            })
            .unwrap();
        for intern in [false, true] {
            if intern {
                index.intern_names().unwrap();
            }
            let prepared = SearchIndex::build(&index);
            for text in [
                "",
                "a",
                "Report.PDF",
                "istan",
                "İ",
                "🙂",
                "missing-xyz",
                "foo",
            ] {
                for mode in [Mode::Contains, Mode::Prefix, Mode::Exact] {
                    for kind in [Kind::Any, Kind::File, Kind::Folder] {
                        for ext in [None, Some("pdf"), Some("tar.gz"), Some("")] {
                            for min_size in [None, Some(0), Some(10)] {
                                for largest in [false, true] {
                                    for limit in [0, 1, 17] {
                                        let query = Query {
                                            text: text.into(),
                                            mode,
                                            kind,
                                            extension: ext.map(String::from),
                                            min_size,
                                            largest,
                                            limit,
                                        };
                                        assert_eq!(
                                            prepared.execute(&query),
                                            query.execute(&index),
                                            "{query:?}, intern={intern}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
