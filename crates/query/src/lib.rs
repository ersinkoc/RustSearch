use rustsearch_core::{DIRECTORY, Index, SIZE_KNOWN};
use std::collections::BinaryHeap;
mod prepared;
pub use prepared::{CacheStats, SearchIndex, cache_path};

#[derive(Clone, Copy, Debug, Default)]
pub enum Mode {
    #[default]
    Contains,
    Prefix,
    Exact,
}
#[derive(Clone, Copy, Debug, Default)]
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
