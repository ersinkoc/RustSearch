use anyhow::{Result, ensure};

#[derive(Debug, Clone)]
pub struct JournalRecord {
    pub id: u64,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: Vec<u16>,
}

/// Decode little-endian records without assuming buffer alignment or casting variable-size structs.
pub fn parse_records(
    mut bytes: &[u8],
    mut visit: impl FnMut(JournalRecord) -> Result<()>,
) -> Result<()> {
    while !bytes.is_empty() {
        ensure!(bytes.len() >= 8, "truncated USN record header");
        let length = u32::from_le_bytes(bytes[..4].try_into()?) as usize;
        let major = u16::from_le_bytes(bytes[4..6].try_into()?);
        ensure!(
            major == 2,
            "unsupported USN record version {major}; prototype requires V2"
        );
        ensure!(
            length >= 60 && length <= bytes.len() && length.is_multiple_of(8),
            "invalid USN record length"
        );
        let record = &bytes[..length];
        let name_len = u16::from_le_bytes(record[56..58].try_into()?) as usize;
        let offset = u16::from_le_bytes(record[58..60].try_into()?) as usize;
        ensure!(
            offset >= 60
                && offset.is_multiple_of(2)
                && name_len > 0
                && name_len.is_multiple_of(2)
                && offset
                    .checked_add(name_len)
                    .is_some_and(|end| end <= length),
            "invalid USN name range"
        );
        visit(JournalRecord {
            id: u64::from_le_bytes(record[8..16].try_into()?),
            parent: u64::from_le_bytes(record[16..24].try_into()?),
            usn: i64::from_le_bytes(record[24..32].try_into()?),
            reason: u32::from_le_bytes(record[40..44].try_into()?),
            attributes: u32::from_le_bytes(record[52..56].try_into()?),
            name: record[offset..offset + name_len]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect(),
        })?;
        bytes = &bytes[length..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Vec<u8> {
        let mut r = vec![0; 64];
        r[..4].copy_from_slice(&64u32.to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&0x1234_0000_0000_1234u64.to_le_bytes());
        r[16..24].copy_from_slice(&5u64.to_le_bytes());
        r[24..32].copy_from_slice(&42i64.to_le_bytes());
        r[56..58].copy_from_slice(&2u16.to_le_bytes());
        r[58..60].copy_from_slice(&60u16.to_le_bytes());
        r[60..62].copy_from_slice(&0xd800u16.to_le_bytes());
        r
    }
    #[test]
    fn preserves_full_file_reference_and_utf16_across_multiple_records() {
        let mut bytes = record();
        bytes.extend(record());
        let mut rows = Vec::new();
        parse_records(&bytes, |r| {
            rows.push(r);
            Ok(())
        })
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, 0x1234_0000_0000_1234);
        assert_eq!(rows[0].name, vec![0xd800]);
        assert_eq!(rows[0].usn, 42);
    }
    #[test]
    fn rejects_truncated_zero_length_odd_name_and_unknown_version() {
        let r = record();
        for n in [1, 7, 59, 63] {
            assert!(parse_records(&r[..n], |_| Ok(())).is_err());
        }
        for (start, value) in [(0, 0u16), (4, 3), (56, 1), (58, 63)] {
            let mut r = record();
            r[start..start + 2].copy_from_slice(&value.to_le_bytes());
            assert!(parse_records(&r, |_| Ok(())).is_err());
        }
    }
}
