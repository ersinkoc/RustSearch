use anyhow::{Result, ensure};
use std::io::Read;

/// Hash exactly the bytes consumed by a decoder, without a separate checksum pass or seek.
/// Callers decode bounded blocks and must finish before exposing any tentative records.
pub struct PayloadReader<R> {
    inner: R,
    hash: crc32fast::Hasher,
}
impl<R: Read> PayloadReader<R> {
    pub fn new(inner: R, prefix: &[u8]) -> Self {
        let mut hash = crc32fast::Hasher::new();
        hash.update(prefix);
        Self { inner, hash }
    }
    pub fn finish(mut self, expected: u32) -> Result<()> {
        let mut extra = [0u8; 1];
        ensure!(self.read(&mut extra)? == 0, "trailing payload bytes");
        ensure!(
            self.hash.finalize() == expected,
            "payload checksum mismatch"
        );
        Ok(())
    }
}
impl<R: Read> Read for PayloadReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(bytes)?;
        self.hash.update(&bytes[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::Cursor, rc::Rc};
    struct ShortRead {
        data: Cursor<Vec<u8>>,
        count: Rc<Cell<usize>>,
    }
    impl Read for ShortRead {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let limit = bytes.len().min(3);
            let n = self.data.read(&mut bytes[..limit])?;
            self.count.set(self.count.get() + n);
            Ok(n)
        }
    }
    #[test]
    fn hashes_forward_only_short_reads_once_including_header_prefix() {
        let data: Vec<u8> = (0..251).cycle().take(10000).collect();
        let count = Rc::new(Cell::new(0));
        let mut expected = b"header".to_vec();
        expected.extend(&data);
        let mut reader = PayloadReader::new(
            ShortRead {
                data: Cursor::new(data.clone()),
                count: count.clone(),
            },
            b"header",
        );
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        reader.finish(crc32fast::hash(&expected)).unwrap();
        assert_eq!(output, data);
        assert_eq!(count.get(), data.len());
    }
    #[test]
    fn rejects_wrong_checksum_trailing_bytes_and_truncation() {
        let mut reader = PayloadReader::new(Cursor::new(b"data"), &[]);
        let mut data = [0; 4];
        reader.read_exact(&mut data).unwrap();
        assert!(
            reader
                .finish(0)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        let mut reader = PayloadReader::new(Cursor::new(b"datatail"), &[]);
        reader.read_exact(&mut data).unwrap();
        assert!(
            reader
                .finish(crc32fast::hash(b"data"))
                .unwrap_err()
                .to_string()
                .contains("trailing")
        );
        let mut reader = PayloadReader::new(Cursor::new(b"dat"), &[]);
        assert!(reader.read_exact(&mut data).is_err());
    }
}
