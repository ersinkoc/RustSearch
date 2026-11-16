use std::{borrow::Cow, collections::TryReserveError, ops::Range};

const BLOCK: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
enum Block {
    Narrow { low: Box<[u8]>, high: Box<[u32]> },
    Wide(Box<[u16]>),
}
impl Block {
    fn encode(units: &[u16]) -> Self {
        let high: Vec<u32> = units
            .iter()
            .enumerate()
            .filter(|(_, unit)| **unit > 255)
            .map(|(at, unit)| ((unit >> 8) as u32) << 12 | at as u32)
            .collect();
        if high.len() * 4 >= units.len() {
            Self::Wide(units.into())
        } else {
            Self::Narrow {
                low: units.iter().map(|unit| *unit as u8).collect(),
                high: high.into_boxed_slice(),
            }
        }
    }
    fn slice(&self, range: Range<usize>) -> Cow<'_, [u16]> {
        match self {
            Self::Wide(units) => Cow::Borrowed(&units[range]),
            Self::Narrow { low, high } => {
                let mut units: Vec<u16> = low[range.clone()].iter().map(|b| *b as u16).collect();
                let start = high.partition_point(|v| (v & 4095) < range.start as u32);
                for &v in &high[start..] {
                    let at = (v & 4095) as usize;
                    if at >= range.end {
                        break;
                    }
                    units[at - range.start] |= ((v >> 12) as u16) << 8;
                }
                Cow::Owned(units)
            }
        }
    }
    fn allocated_bytes(&self) -> usize {
        match self {
            Self::Narrow { low, high } => low.len() + high.len() * 4,
            Self::Wide(units) => units.len() * 2,
        }
    }
}

/// Lossless UTF-16 blocks. Latin-1 low bytes plus sparse high bytes avoid paying
/// two bytes for every ASCII name. Dense non-Latin blocks stay plain UTF-16.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Names {
    blocks: Vec<Block>,
    tail: Vec<u16>,
    len: usize,
}
impl Names {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.blocks.try_reserve_exact(additional.div_ceil(BLOCK))?;
        self.tail
            .try_reserve_exact(BLOCK.saturating_sub(self.tail.len()))
    }
    pub fn extend_from_slice(&mut self, mut units: &[u16]) {
        self.len += units.len();
        while !units.is_empty() {
            let n = units.len().min(BLOCK - self.tail.len());
            self.tail.extend_from_slice(&units[..n]);
            units = &units[n..];
            if self.tail.len() == BLOCK {
                self.blocks.push(Block::encode(&self.tail));
                self.tail.clear();
            }
        }
    }
    pub fn append(&mut self, other: &Self) {
        for units in other.decoded_blocks() {
            self.extend_from_slice(&units);
        }
    }
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, [u16]> {
        assert!(range.start <= range.end && range.end <= self.len);
        if range.is_empty() {
            return Cow::Borrowed(&[]);
        }
        let first = range.start / BLOCK;
        let last = (range.end - 1) / BLOCK;
        if first == last {
            let local = range.start % BLOCK..(range.end - 1) % BLOCK + 1;
            return if first == self.blocks.len() {
                Cow::Borrowed(&self.tail[local])
            } else {
                self.blocks[first].slice(local)
            };
        }
        let mut result = Vec::with_capacity(range.len());
        for block in first..=last {
            let start = range.start.saturating_sub(block * BLOCK);
            let end = (range.end - block * BLOCK).min(BLOCK);
            if block == self.blocks.len() {
                result.extend_from_slice(&self.tail[start..end]);
            } else {
                result.extend_from_slice(&self.blocks[block].slice(start..end));
            }
        }
        Cow::Owned(result)
    }
    pub fn decoded_blocks(&self) -> impl Iterator<Item = Cow<'_, [u16]>> {
        self.blocks
            .iter()
            .map(|block| block.slice(0..BLOCK))
            .chain(std::iter::once(Cow::Borrowed(self.tail.as_slice())).filter(|b| !b.is_empty()))
    }
    pub fn shrink_to_fit(&mut self) {
        self.blocks.shrink_to_fit();
        self.tail.shrink_to_fit();
    }
    pub fn allocated_bytes(&self) -> usize {
        self.blocks.capacity() * std::mem::size_of::<Block>()
            + self.tail.capacity() * 2
            + self
                .blocks
                .iter()
                .map(Block::allocated_bytes)
                .sum::<usize>()
    }
}
impl Extend<u16> for Names {
    fn extend<T: IntoIterator<Item = u16>>(&mut self, iter: T) {
        for unit in iter {
            self.extend_from_slice(&[unit]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_append_and_block_boundaries_preserve_every_utf16_unit() {
        let mut original = vec![65u16; BLOCK * 3 + 33];
        for (at, unit) in [
            (0, 0),
            (9, 255),
            (4095, 0xd800),
            (4096, 0xdc00),
            (8191, 0x100),
            (12290, 0xffff),
        ] {
            original[at] = unit;
        }
        let mut names = Names::default();
        for chunk in original.chunks(113) {
            names.extend_from_slice(chunk);
        }
        for start in [0, 1, 9, 4094, 4095, 4096, 8190, 12288, original.len()] {
            for length in [0, 1, 3, 4096, original.len()] {
                let end = start.saturating_add(length).min(original.len());
                assert_eq!(names.slice(start..end).as_ref(), &original[start..end]);
            }
        }
        let mut appended = Names::default();
        appended.extend_from_slice(&[0x1234, 0xdfff]);
        appended.append(&names);
        let mut expected = vec![0x1234, 0xdfff];
        expected.extend(&original);
        assert_eq!(appended.slice(0..appended.len()).as_ref(), expected);
        assert_eq!(
            names
                .decoded_blocks()
                .flat_map(|b| b.into_owned())
                .collect::<Vec<_>>(),
            original
        );
    }

    #[test]
    fn dense_names_preserve_all_65536_units_and_sparse_names_use_less_memory() {
        let units: Vec<u16> = (0..=u16::MAX).collect();
        let mut dense = Names::default();
        dense.extend_from_slice(&units);
        assert_eq!(dense.slice(0..units.len()).as_ref(), units);
        let mut sparse = Names::default();
        sparse.extend_from_slice(&vec![120; 65536]);
        assert!(sparse.allocated_bytes() < 65536 * 3 / 2);
    }
}
