//! 读取 ONNX Runtime 的 `.ort` 平面缓冲文件。
//!
//! 上游 `export_step.py` 发布的是 ORT 格式,不是 ONNX protobuf。这里只实现
//! 取权重所需的那一小部分 schema:`InferenceSession.model.graph` 里的
//! `initializers` 与 `nodes`。所有偏移都来自文件,读取前逐项检查边界。

use std::collections::BTreeMap;

use crate::lm::ModelError;

pub(crate) enum OrtTensor {
    Float(Vec<f32>),
    Uint8(Vec<u8>),
}

pub(crate) struct OrtWeight {
    pub shape: Vec<usize>,
    pub data: OrtTensor,
}

pub(crate) struct OrtModel {
    pub weights: BTreeMap<String, OrtWeight>,
    /// 偏置张量名 → 动态量化矩阵的基名(不含 `_quantized`)。
    ///
    /// 图的节点里,`DynamicQuantizeMatMul` 的第五个输入是偏置,名字稳定地
    /// 形如 `model.blocks.0.qkv.bias`;权重本身的编号由导出器生成,不能靠
    /// 名字硬匹配。
    pub matmuls: BTreeMap<String, String>,
    /// `DequantizeLinear` 的零点张量名 → 缩放张量名。
    ///
    /// 25M 的 `model.chars.weight_quantized` 没有同名的 `_scale`,图里复用了
    /// 输出投影那份缩放;零点名字仍然唯一,可以据此找到。
    pub dequant_scales: BTreeMap<String, String>,
}

struct Vector<'a> {
    data: &'a [u8],
    len: usize,
}

impl<'a> Vector<'a> {
    fn bytes(&self, index: usize, size: usize) -> Result<&'a [u8], ModelError> {
        let start = index
            .checked_mul(size)
            .ok_or_else(|| bad("vector offset overflows"))?;
        let end = start
            .checked_add(size)
            .ok_or_else(|| bad("vector offset overflows"))?;
        self.data.get(start..end).ok_or(ModelError::Truncated)
    }

    fn u32(&self, index: usize) -> Result<u32, ModelError> {
        let bytes = self.bytes(index, 4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i64(&self, index: usize) -> Result<i64, ModelError> {
        let bytes = self.bytes(index, 8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn table(&self, index: usize) -> Result<Table<'a>, ModelError> {
        let element = self
            .data
            .get(
                index
                    .checked_mul(4)
                    .ok_or_else(|| bad("vector offset overflows"))?..,
            )
            .ok_or(ModelError::Truncated)?;
        let position = u32_at(element, 0)? as usize;
        let target = index
            .checked_mul(4)
            .and_then(|offset| offset.checked_add(position))
            .ok_or_else(|| bad("table offset overflows"))?;
        Table::at(self.data, target)
    }
}

struct Table<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Table<'a> {
    fn at(data: &'a [u8], pos: usize) -> Result<Self, ModelError> {
        if pos >= data.len() {
            return Err(ModelError::Truncated);
        }
        Ok(Self { data, pos })
    }

    fn field(&self, index: usize) -> Result<Option<usize>, ModelError> {
        let vtable = self.pos as i64 - i64::from(i32_at(self.data, self.pos)?);
        let vtable = usize::try_from(vtable).map_err(|_| bad("vtable lies before the buffer"))?;
        let size = usize::from(u16_at(self.data, vtable)?);
        let slot = 4usize
            .checked_add(index.checked_mul(2).ok_or_else(|| bad("field overflows"))?)
            .ok_or_else(|| bad("field overflows"))?;
        match slot.checked_add(2) {
            Some(end) if end <= size => {}
            _ => return Ok(None),
        }
        let field = vtable
            .checked_add(slot)
            .ok_or_else(|| bad("field overflows"))?;
        let offset = usize::from(u16_at(self.data, field)?);
        match (offset != 0).then(|| self.pos.checked_add(offset)) {
            Some(Some(position)) => Ok(Some(position)),
            Some(None) => Err(bad("field offset overflows")),
            None => Ok(None),
        }
    }

    fn i32(&self, index: usize) -> Result<Option<i32>, ModelError> {
        self.field(index)?
            .map(|position| i32_at(self.data, position))
            .transpose()
    }

    fn string(&self, index: usize) -> Result<Option<String>, ModelError> {
        let Some(position) = self.field(index)? else {
            return Ok(None);
        };
        let start = position
            .checked_add(u32_at(self.data, position)? as usize)
            .ok_or_else(|| bad("string offset overflows"))?;
        let len = u32_at(self.data, start)? as usize;
        let body = start
            .checked_add(4)
            .ok_or_else(|| bad("string length overflows"))?;
        let end = body
            .checked_add(len)
            .ok_or_else(|| bad("string length overflows"))?;
        let bytes = self.data.get(body..end).ok_or(ModelError::Truncated)?;
        let value = std::str::from_utf8(bytes).map_err(|error| bad(error.to_string()))?;
        Ok(Some(value.to_owned()))
    }

    fn table(&self, index: usize) -> Result<Option<Table<'a>>, ModelError> {
        let Some(position) = self.field(index)? else {
            return Ok(None);
        };
        let target = position
            .checked_add(u32_at(self.data, position)? as usize)
            .ok_or_else(|| bad("table offset overflows"))?;
        Ok(Some(Table::at(self.data, target)?))
    }

    fn vector(&self, index: usize) -> Result<Option<Vector<'a>>, ModelError> {
        let Some(position) = self.field(index)? else {
            return Ok(None);
        };
        let start = position
            .checked_add(u32_at(self.data, position)? as usize)
            .ok_or_else(|| bad("vector offset overflows"))?;
        let len = u32_at(self.data, start)? as usize;
        let body = start
            .checked_add(4)
            .ok_or_else(|| bad("vector length overflows"))?;
        let data = self.data.get(body..).ok_or(ModelError::Truncated)?;
        Ok(Some(Vector { data, len }))
    }

    fn strings(&self, index: usize) -> Result<Vec<String>, ModelError> {
        let Some(vector) = self.vector(index)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(vector.len);
        for element in 0..vector.len {
            let relative = vector.u32(element)? as usize;
            let offset = element
                .checked_mul(4)
                .and_then(|start| start.checked_add(relative))
                .ok_or_else(|| bad("string offset overflows"))?;
            let len = u32_at(vector.data, offset)? as usize;
            let start = offset
                .checked_add(4)
                .ok_or_else(|| bad("string length overflows"))?;
            let end = start
                .checked_add(len)
                .ok_or_else(|| bad("string length overflows"))?;
            let bytes = vector.data.get(start..end).ok_or(ModelError::Truncated)?;
            out.push(
                std::str::from_utf8(bytes)
                    .map_err(|error| bad(error.to_string()))?
                    .to_owned(),
            );
        }
        Ok(out)
    }
}

impl OrtModel {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.get(4..8) != Some(&b"ORTM"[..]) {
            return Err(bad("not an ORT flatbuffer"));
        }
        let root = Table::at(bytes, u32_at(bytes, 0)? as usize)?;
        let model = root.table(1)?.ok_or_else(|| bad("ORT file has no model"))?;
        let graph = model
            .table(7)?
            .ok_or_else(|| bad("ORT model has no graph"))?;

        let mut weights = BTreeMap::new();
        if let Some(initializers) = graph.vector(0)? {
            for index in 0..initializers.len {
                let tensor = initializers.table(index)?;
                let Some(name) = tensor.string(0)? else {
                    continue;
                };
                let shape = match tensor.vector(2)? {
                    Some(dims) => {
                        let mut shape = Vec::with_capacity(dims.len);
                        for dimension in 0..dims.len {
                            let value = dims.i64(dimension)?;
                            shape.push(
                                usize::try_from(value)
                                    .map_err(|_| bad(format!("{name}: negative dimension")))?,
                            );
                        }
                        shape
                    }
                    None => Vec::new(),
                };
                let count = shape
                    .iter()
                    .try_fold(1usize, |total, dimension| total.checked_mul(*dimension))
                    .ok_or_else(|| bad(format!("{name}: shape overflows")))?;
                let Some(raw) = tensor.vector(4)? else {
                    continue;
                };
                let data = match tensor.i32(3)?.unwrap_or(0) {
                    // 1 = FLOAT
                    1 => {
                        let bytes = raw.bytes(
                            0,
                            count
                                .checked_mul(4)
                                .ok_or_else(|| bad(format!("{name}: raw data overflows")))?,
                        )?;
                        OrtTensor::Float(
                            bytes
                                .chunks_exact(4)
                                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                .collect(),
                        )
                    }
                    // 2 = UINT8
                    2 => OrtTensor::Uint8(raw.bytes(0, count)?.to_vec()),
                    // int64 常量与推理无关。
                    _ => continue,
                };
                weights.insert(name, OrtWeight { shape, data });
            }
        }

        let mut matmuls = BTreeMap::new();
        let mut dequant_scales = BTreeMap::new();
        if let Some(nodes) = graph.vector(2)? {
            for index in 0..nodes.len {
                let node = nodes.table(index)?;
                let Some(op) = node.string(5)? else {
                    continue;
                };
                let inputs = node.strings(8)?;
                if op == "DynamicQuantizeMatMul" {
                    if inputs.len() < 5 || !inputs[4].starts_with("model.blocks.") {
                        continue;
                    }
                    matmuls.insert(
                        inputs[4].clone(),
                        inputs[1]
                            .strip_suffix("_quantized")
                            .unwrap_or(&inputs[1])
                            .to_owned(),
                    );
                } else if op == "DequantizeLinear" && inputs.len() >= 3 {
                    dequant_scales.insert(inputs[2].clone(), inputs[1].clone());
                }
            }
        }

        Ok(Self {
            weights,
            matmuls,
            dequant_scales,
        })
    }

    pub(crate) fn weight(&self, name: &str) -> Result<&OrtWeight, ModelError> {
        self.weights
            .get(name)
            .ok_or_else(|| ModelError::MissingTensor(name.to_owned()))
    }
}

fn bad(message: impl Into<String>) -> ModelError {
    ModelError::Header(message.into())
}

fn u16_at(data: &[u8], at: usize) -> Result<u16, ModelError> {
    let end = at.checked_add(2).ok_or(ModelError::Truncated)?;
    let bytes = data.get(at..end).ok_or(ModelError::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, ModelError> {
    let end = at.checked_add(4).ok_or(ModelError::Truncated)?;
    let bytes = data.get(at..end).ok_or(ModelError::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn i32_at(data: &[u8], at: usize) -> Result<i32, ModelError> {
    let end = at.checked_add(4).ok_or(ModelError::Truncated)?;
    let bytes = data.get(at..end).ok_or(ModelError::Truncated)?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
