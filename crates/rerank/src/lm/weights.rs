//! Reads the safetensors file `neural/export.py` writes. The format is specified in
//! `docs/format.md`.
//!
//! The layout is a little-endian `u64` header length, that many bytes of JSON describing every
//! tensor, then the tensor data. Offsets in the header are relative to the start of the data
//! section. Parsing it here rather than through a library keeps the crate free of dependencies
//! outside the ones the workspace already carries.

use std::collections::BTreeMap;
use std::ops::Deref;
#[cfg(windows)]
use std::{fs::File, ops::Range, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::lm::ModelError;

#[derive(Deserialize, Serialize)]
struct Entry {
    dtype: String,
    shape: Vec<usize>,
    data_offsets: [usize; 2],
}

/// One tensor as it will be held for the life of the model.
///
/// Quantized matrices keep their `i8` values and their per-row scales instead of being expanded to
/// `f32` on load. The scale is a constant factor for a whole output row, so it comes out of the dot
/// product and is applied once to the result — the arithmetic is the same, and the weights occupy a
/// quarter of the memory. That matters because the model is loaded inside an iOS keyboard
/// extension, where expanding a 4.5 MB file to 24.5 MB resident is most of the process budget.
pub enum Tensor {
    Float(Vec<f32>),
    Quantized { values: Int8, scales: Vec<f32> },
}

pub enum Int8 {
    Owned(Vec<i8>),
    #[cfg(windows)]
    Mapped(Arc<Mapping>, Range<usize>),
}

impl From<Vec<i8>> for Int8 {
    fn from(values: Vec<i8>) -> Self {
        Self::Owned(values)
    }
}

impl Deref for Int8 {
    type Target = [i8];

    #[allow(unsafe_code)]
    fn deref(&self) -> &[i8] {
        match self {
            Self::Owned(values) => values,
            #[cfg(windows)]
            Self::Mapped(map, range) => {
                let bytes = &map.bytes[range.clone()];
                // i8 and u8 have identical alignment and admit every byte pattern.
                unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len()) }
            }
        }
    }
}

pub(crate) const PACKED_FORMAT: &str = "lueying-rerank-packed-v1";

#[cfg(windows)]
pub(crate) struct Mapping {
    bytes: memmap2::Mmap,
    _file: File,
}

#[cfg(windows)]
impl Mapping {
    #[allow(unsafe_code)]
    pub(crate) fn open(path: &std::path::Path) -> std::io::Result<Arc<Self>> {
        use std::os::windows::fs::OpenOptionsExt;

        // Deny writers for the entire life of the view. Updates can replace the file by rename.
        const FILE_SHARE_READ_DELETE: u32 = 0x1 | 0x4;
        let file = File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ_DELETE)
            .open(path)?;
        let bytes = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Arc::new(Self { bytes, _file: file }))
    }

    pub(crate) fn warm(&self) {
        let sum = self
            .bytes
            .iter()
            .step_by(4096)
            .fold(0usize, |sum, &byte| sum + usize::from(byte));
        std::hint::black_box(sum);
    }
}

impl Tensor {
    pub fn len(&self) -> usize {
        match self {
            Tensor::Float(values) => values.len(),
            Tensor::Quantized { values, .. } => values.len(),
        }
    }
}

/// Every tensor as stored, plus the metadata the header carried.
pub struct Weights {
    tensors: BTreeMap<String, (Vec<usize>, Tensor)>,
    metadata: BTreeMap<String, String>,
}

impl Weights {
    pub fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        Self::parse_source(bytes, None)
    }

    #[cfg(windows)]
    pub fn parse_mapped(map: &Arc<Mapping>) -> Result<Self, ModelError> {
        Self::parse_source(&map.bytes, Some(map))
    }

    fn parse_source(
        bytes: &[u8],
        #[cfg(windows)] map: Option<&Arc<Mapping>>,
        #[cfg(not(windows))] _map: Option<()>,
    ) -> Result<Self, ModelError> {
        let length_bytes: [u8; 8] = bytes
            .get(..8)
            .ok_or(ModelError::Truncated)?
            .try_into()
            .map_err(|_| ModelError::Truncated)?;
        // The length comes from the file, so it can be anything. Adding it to the eight byte prefix
        // without checking overflows on a hostile value, and an overflow here is a panic in a debug
        // build and a wrapped index in a release one.
        let header_len =
            usize::try_from(u64::from_le_bytes(length_bytes)).map_err(|_| ModelError::Truncated)?;
        let header_end = header_len.checked_add(8).ok_or(ModelError::Truncated)?;
        let header = bytes.get(8..header_end).ok_or(ModelError::Truncated)?;
        let data = bytes.get(header_end..).ok_or(ModelError::Truncated)?;

        let mut raw: BTreeMap<String, serde_json::Value> = serde_json::from_slice(header)
            .map_err(|error| ModelError::Header(error.to_string()))?;
        let metadata = match raw.remove("__metadata__") {
            Some(value) => serde_json::from_value(value)
                .map_err(|error| ModelError::Header(error.to_string()))?,
            None => BTreeMap::new(),
        };

        // Scales are consumed while dequantizing the tensor they belong to, so they are collected
        // first and never become entries of their own.
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (name, value) in raw {
            let entry: Entry = serde_json::from_value(value)
                .map_err(|error| ModelError::Header(format!("{name}: {error}")))?;
            entries.insert(name, entry);
        }

        let mut tensors = BTreeMap::new();
        for (name, entry) in &entries {
            if name.ends_with(".scale") {
                continue;
            }
            let values = match entry.dtype.as_str() {
                "F32" => Tensor::Float(read_f32(data, entry)?),
                "F16" => Tensor::Float(read_f16(data, entry)?),
                "I8" => {
                    let scale_entry = entries
                        .get(&format!("{name}.scale"))
                        .ok_or_else(|| ModelError::Header(format!("{name}: missing scale")))?;
                    let scales = read_f32(data, scale_entry)?;
                    read_i8(data, entry, scales, |bytes| {
                        #[cfg(windows)]
                        if let Some(map) = map {
                            let [start, end] = entry.data_offsets;
                            return Int8::Mapped(
                                Arc::clone(map),
                                header_end + start..header_end + end,
                            );
                        }
                        Int8::Owned(bytes.iter().map(|&byte| byte as i8).collect())
                    })?
                }
                other => return Err(ModelError::Header(format!("{name}: dtype {other}"))),
            };
            // The shape comes from the file; multiplying it out unchecked overflows on a hostile
            // one, and the product is then compared against a real length and happens to match.
            let expected = entry
                .shape
                .iter()
                .try_fold(1usize, |total, dimension| total.checked_mul(*dimension))
                .ok_or_else(|| ModelError::Header(format!("{name}: shape overflows")))?;
            if values.len() != expected {
                return Err(ModelError::Header(format!(
                    "{name}: shape does not match data"
                )));
            }
            tensors.insert(name.clone(), (entry.shape.clone(), values));
        }

        Ok(Self { tensors, metadata })
    }

    /// Takes a tensor that must be plain `f32`: layer-norm parameters, biases and the position
    /// table, which `export.py` never quantizes because they are a negligible share of the file and
    /// the most sensitive to rounding.
    pub fn take(&mut self, name: &str) -> Result<Vec<f32>, ModelError> {
        match self.take_tensor(name)? {
            Tensor::Float(values) => Ok(values),
            Tensor::Quantized { .. } => Err(ModelError::Header(format!(
                "{name} is quantized, but this tensor is never quantized by export.py"
            ))),
        }
    }

    pub fn take_tensor(&mut self, name: &str) -> Result<Tensor, ModelError> {
        self.tensors
            .remove(name)
            .map(|(_, values)| values)
            .ok_or_else(|| ModelError::MissingTensor(name.to_owned()))
    }

    pub fn take_optional(&mut self, name: &str) -> Result<Option<Vec<f32>>, ModelError> {
        self.tensors
            .contains_key(name)
            .then(|| self.take(name))
            .transpose()
    }

    pub fn metadata(&self, key: &str) -> Result<&str, ModelError> {
        self.metadata
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| ModelError::MissingMetadata(key.to_owned()))
    }
}

#[derive(Default)]
pub(crate) struct Pack {
    header: BTreeMap<String, serde_json::Value>,
    data: Vec<u8>,
}

impl Pack {
    fn tensor(
        &mut self,
        name: &str,
        dtype: &str,
        shape: &[usize],
        bytes: impl Iterator<Item = u8>,
    ) {
        let start = self.data.len();
        self.data.extend(bytes);
        self.header.insert(
            name.into(),
            serde_json::json!(Entry {
                dtype: dtype.into(),
                shape: shape.to_vec(),
                data_offsets: [start, self.data.len()],
            }),
        );
    }

    pub(crate) fn float(&mut self, name: &str, shape: &[usize], values: &[f32]) {
        self.tensor(
            name,
            "F32",
            shape,
            values.iter().flat_map(|v| v.to_le_bytes()),
        );
    }

    pub(crate) fn matrix(&mut self, name: &str, shape: [usize; 2], tensor: &Tensor) {
        match tensor {
            Tensor::Float(values) => self.float(name, &shape, values),
            Tensor::Quantized { values, scales } => {
                self.tensor(name, "I8", &shape, values.iter().map(|&v| v as u8));
                self.float(&format!("{name}.scale"), &[scales.len()], scales);
            }
        }
    }

    pub(crate) fn write(
        mut self,
        out: &mut impl std::io::Write,
        metadata: BTreeMap<&str, String>,
    ) -> std::io::Result<()> {
        self.header
            .insert("__metadata__".into(), serde_json::json!(metadata));
        let mut header = serde_json::to_vec(&self.header)?;
        header.resize(header.len().next_multiple_of(8), b' ');
        out.write_all(&(header.len() as u64).to_le_bytes())?;
        out.write_all(&header)?;
        out.write_all(&self.data)
    }
}

fn slice<'a>(data: &'a [u8], entry: &Entry) -> Result<&'a [u8], ModelError> {
    data.get(entry.data_offsets[0]..entry.data_offsets[1])
        .ok_or(ModelError::Truncated)
}

fn read_f32(data: &[u8], entry: &Entry) -> Result<Vec<f32>, ModelError> {
    let bytes = slice(data, entry)?;
    if entry.dtype != "F32" || !bytes.len().is_multiple_of(4) {
        return Err(ModelError::Header("invalid float32 tensor".into()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

fn read_f16(data: &[u8], entry: &Entry) -> Result<Vec<f32>, ModelError> {
    Ok(slice(data, entry)?
        .chunks_exact(2)
        .map(|b| half_to_f32(u16::from_le_bytes([b[0], b[1]])))
        .collect())
}

/// Quantization is symmetric and per output row, so the scale index is the row index.
fn read_i8(
    data: &[u8],
    entry: &Entry,
    scales: Vec<f32>,
    store: impl FnOnce(&[u8]) -> Int8,
) -> Result<Tensor, ModelError> {
    let bytes = slice(data, entry)?;
    let rows = *entry.shape.first().unwrap_or(&1);
    if rows == 0 || scales.len() != rows {
        return Err(ModelError::Header(
            "scale length does not match rows".into(),
        ));
    }
    Ok(Tensor::Quantized {
        values: store(bytes),
        scales,
    })
}

/// IEEE 754 binary16 to binary32. Subnormals are rare in trained weights but are handled rather
/// than flushed, because silently mapping them to zero would be a difference the caller cannot see.
fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x03ff);
    if exponent == 0 {
        if mantissa == 0 {
            return f32::from_bits(sign);
        }
        // Renormalize. A subnormal half is `mantissa * 2^-24`, so its leading set bit sits at
        // `31 - leading_zeros` and the unbiased exponent is that index minus 24.
        let leading = mantissa.leading_zeros();
        let exponent = 134 - leading;
        let mantissa = (mantissa << (leading - 8)) & 0x007f_ffff;
        return f32::from_bits(sign | (exponent << 23) | mantissa);
    }
    if exponent == 0x1f {
        return f32::from_bits(sign | 0x7f80_0000 | (mantissa << 13));
    }
    f32::from_bits(sign | ((exponent + 127 - 15) << 23) | (mantissa << 13))
}

#[cfg(test)]
mod tests {
    use super::half_to_f32;

    #[test]
    fn converts_representative_halves() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x8000), -0.0);
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x3555), 0.333_251_95);
        // Subnormals, which the renormalizing branch has to reach. A subnormal half is exactly
        // `mantissa * 2^-24`, and every such value is representable in f32, so these are exact.
        assert_eq!(half_to_f32(0x03ff), 1023.0 / 16_777_216.0);
        assert_eq!(half_to_f32(0x0001), 1.0 / 16_777_216.0);
        assert_eq!(half_to_f32(0x0200), 512.0 / 16_777_216.0);
        assert_eq!(half_to_f32(0x8001), -1.0 / 16_777_216.0);
        assert!(half_to_f32(0x7c00).is_infinite());
        assert!(half_to_f32(0x7e00).is_nan());
    }
}
