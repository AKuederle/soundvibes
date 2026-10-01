use half::f16;
use serde::Deserialize;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{Error, Result};

#[derive(Deserialize)]
struct Header {
    format: String,
    index: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    n: String,
    k: String,
    shape: Vec<usize>,
    b: usize,
}

pub(crate) struct Record {
    kind: String,
    shape: Vec<usize>,
    bytes: Vec<u8>,
}

pub(crate) struct Container(BTreeMap<String, Record>);

impl Container {
    pub fn read(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(|e| Error::path(path, e))?;
        let size = file.metadata()?.len();
        let mut prefix = [0; 8];
        file.read_exact(&mut prefix)?;
        let header_size = u64::from_le_bytes(prefix);
        if header_size > 4 * 1024 * 1024 || header_size > size.saturating_sub(8) {
            return Err(Error::new("invalid Phonon-2 container header length"));
        }
        let mut bytes = vec![0; header_size as usize];
        file.read_exact(&mut bytes)?;
        let header: Header = serde_json::from_slice(&bytes)?;
        if header.format != "fermion-five-value-parakeet-v1" {
            return Err(Error::new(format!(
                "unsupported container format: {}",
                header.format
            )));
        }
        let mut remaining = size - 8 - header_size;
        let mut records = BTreeMap::new();
        for entry in header.index {
            let elements = entry
                .shape
                .iter()
                .try_fold(1usize, |n, d| n.checked_mul(*d))
                .ok_or_else(|| Error::new("tensor shape overflow"))?;
            if elements > 64 * 1024 * 1024 || elements == 0 || entry.b as u64 > remaining {
                return Err(Error::new(format!("invalid tensor size: {}", entry.n)));
            }
            let valid = match entry.k.as_str() {
                "fp16" => entry.b == elements * 2,
                "int6" => {
                    !entry.shape.is_empty()
                        && entry.b == elements.div_ceil(4) * 3 + entry.shape[0] * 2
                }
                "five_value" => {
                    entry.shape.len() == 2
                        && entry.b
                            >= entry.shape[0] * entry.shape[1].div_ceil(5) + 4 * entry.shape[0]
                        && entry.b
                            <= entry.shape[0] * entry.shape[1].div_ceil(5)
                                + elements.div_ceil(8)
                                + 4 * entry.shape[0]
                }
                _ => false,
            };
            if !valid {
                return Err(Error::new(format!(
                    "invalid {} record: {}",
                    entry.k, entry.n
                )));
            }
            let mut bytes = vec![0; entry.b];
            file.read_exact(&mut bytes)?;
            remaining -= entry.b as u64;
            if records
                .insert(
                    entry.n.clone(),
                    Record {
                        kind: entry.k,
                        shape: entry.shape,
                        bytes,
                    },
                )
                .is_some()
            {
                return Err(Error::new(format!("duplicate tensor: {}", entry.n)));
            }
        }
        if remaining != 0 {
            return Err(Error::new("trailing bytes in Phonon-2 container"));
        }
        Ok(Self(records))
    }

    fn record(&self, name: &str, shape: &[usize]) -> Result<&Record> {
        let record = self
            .0
            .get(name)
            .ok_or_else(|| Error::new(format!("missing tensor: {name}")))?;
        if record.shape != shape {
            return Err(Error::new(format!(
                "invalid shape for {name}: {:?}, expected {shape:?}",
                record.shape
            )));
        }
        Ok(record)
    }

    pub fn dense(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>> {
        let record = self.record(name, shape)?;
        match record.kind.as_str() {
            "fp16" => Ok(halves(&record.bytes)
                .into_iter()
                .map(|v| f16::from_bits(v).to_f32())
                .collect()),
            "int6" => {
                let q = self.quantized(name, shape)?;
                let cols = shape.iter().skip(1).product::<usize>();
                Ok(q.values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| *v as f32 * f16::from_bits(q.scales[i / cols]).to_f32())
                    .collect())
            }
            _ => Err(Error::new(format!("expected dense tensor: {name}"))),
        }
    }

    pub fn half(&self, name: &str, shape: &[usize]) -> Result<Vec<u16>> {
        let record = self.record(name, shape)?;
        if record.kind != "fp16" {
            return Err(Error::new(format!("expected fp16 tensor: {name}")));
        }
        Ok(halves(&record.bytes))
    }

    pub fn quantized(&self, name: &str, shape: &[usize]) -> Result<Quantized> {
        let record = self.record(name, shape)?;
        if record.kind != "int6" {
            return Err(Error::new(format!("expected int6 tensor: {name}")));
        }
        let count = shape.iter().product::<usize>();
        let body_size = count.div_ceil(4) * 3;
        let mut values = Vec::with_capacity(count);
        for chunk in record.bytes[..body_size].as_chunks::<3>().0 {
            let packed = chunk[0] as u32 | (chunk[1] as u32) << 8 | (chunk[2] as u32) << 16;
            for shift in [0, 6, 12, 18] {
                values.push(((packed >> shift) & 63) as i8 - 32);
            }
        }
        values.truncate(count);
        Ok(Quantized {
            values,
            scales: halves(&record.bytes[body_size..]),
        })
    }

    pub fn planes(&self, name: &str, rows: usize, cols: usize) -> Result<Planes> {
        let record = self.record(name, &[rows, cols])?;
        if record.kind != "five_value" || !cols.is_multiple_of(64) {
            return Err(Error::new(format!("unsupported packed matrix: {name}")));
        }
        let row_bytes = cols.div_ceil(5);
        let signs_size = rows * row_bytes;
        let mut a = vec![0; rows * cols / 4];
        let mut b = a.clone();
        let mut nonzero = 0usize;
        for row in 0..rows {
            for col in 0..cols {
                let trit = (record.bytes[row * row_bytes + col / 5] as usize
                    / 3usize.pow((col % 5) as u32))
                    % 3;
                a[row * cols / 4 + col / 4] |= (trit as u8) << (2 * (col % 4));
                if trit != 1 {
                    nonzero += 1;
                }
            }
        }
        let selector_size = nonzero.div_ceil(8);
        if signs_size + selector_size + rows * 4 != record.bytes.len() {
            return Err(Error::new(format!("invalid five-value selectors: {name}")));
        }
        let mut bit = 0;
        for row in 0..rows {
            for col in 0..cols {
                let i = row * cols / 4 + col / 4;
                let shift = 2 * (col % 4);
                let trit = (a[i] >> shift) & 3;
                let high = trit != 1 && (record.bytes[signs_size + bit / 8] >> (bit % 8)) & 1 != 0;
                b[i] |= (if high { trit } else { 1 }) << shift;
                bit += usize::from(trit != 1);
            }
        }
        let off = signs_size + selector_size;
        Ok(Planes {
            a,
            b,
            lo: halves(&record.bytes[off..off + rows * 2]),
            hi: halves(&record.bytes[off + rows * 2..]),
        })
    }
}

fn halves(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect()
}

pub(crate) struct Quantized {
    pub values: Vec<i8>,
    pub scales: Vec<u16>,
}

pub(crate) struct Planes {
    pub a: Vec<u8>,
    pub b: Vec<u8>,
    pub lo: Vec<u16>,
    pub hi: Vec<u16>,
}
