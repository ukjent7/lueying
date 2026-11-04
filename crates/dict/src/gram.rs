//! librime-octagram `.gram` 语言模型:mmap 零解析 + darts-clone 双数组查询。
//!
//! 格式(与 librime `gram_db.cc` 一致):44 字节 metadata(`format[32]`、
//! `checksum`、`double_array_size`、`OffsetPtr`)后紧跟 4 字节 unit 的双数组。
//! 每个键是「前文尾部 ≤N 字 + 当前词首部 ≤N 字」的字级编码串,值是该搭配的
//! `ln(频次) × 10000`。查询只做 `traverse` + `commonPrefixSearch`,所以文件可以
//! 直接映射、不反序列化。
//!
//! 查询复刻 librime-octagram 的 `Query`:取前文尾部与当前词首部,逐级缩短前文,
//! 命中搭配取最高分,句末再查一次 `$`。打分不直接照搬 librime 的固定罚,而是
//! 用 `weight × (命中值 - baseline)` 做量纲标定:未命中恰好是 `-weight × baseline`,
//! 命中项围绕零点上下浮动。librime 的 `-12` 同时充当「每转移固定罚」,而我们的
//! 词节点分里已经有 `WORD_PENALTY`,再叠一份会把长句按词数压垮。
//!
//! 模型的键空间是**字**而不是词:调用方传入前文尾部与当前词,本模块按 librime
//! 的规则截取前文尾部与词首部再查表。词级模型(`bgw`)与字级模型(`bgc`)共用这一
//! 套读取、查询与打分代码,差别只在训练数据。

use std::fs::File;
use std::io;
use std::path::Path;

use memmap2::Mmap;

/// metadata 固定 44 字节。
const META: usize = 44;
/// `OffsetPtr` 自身在文件中的偏移;它存的是相对自己地址的位移。
const OFFSET_PTR_AT: usize = 40;
/// darts-clone 每个 unit 4 字节。
const UNIT: usize = 4;
/// 一次编码最多几个 Unicode 字符。
const MAX_CHARS: usize = 8;
/// 查搭配时前文与词各取几个字。前文由调用方限制在 [`CONTEXT_CHARS`] 字;
/// 词首取 5 字能多覆盖一部分四字词/短语,实测比 3 字略好。
const QUERY_CHARS: usize = 5;
/// 调用方提供的前文尾长。模型键以字为单位,3 字上下文已覆盖绝大多数搭配。
pub const CONTEXT_CHARS: usize = 3;
/// 搭配总长不足此值且非整串匹配时,额外扣弱搭配罚。
const MIN_COLLOCATION_CHARS: usize = 2;
/// 弱搭配额外罚,相对命中零点。
const WEAK_COLLOCATION_PENALTY: f64 = -12.0;
/// 句末搭配额外罚,相对命中零点。
const REAR_PENALTY: f64 = -6.0;
/// 整体权重。0 会退回纯词频;这里取句级语料网格搜索的中间档。
///
/// 权重同时决定未命中的代价(`-weight × baseline`)与命中的增益幅度。
/// 0.3~0.4 在 clean/extra/frost/neutral 四份语料上的总准确率差异不到
/// 0.6 个百分点,取 0.35 避免把单份语料的波动当成普遍规律。
const WEIGHT: f64 = 0.35;
/// 命中零点。8.34 是同类字级 `.gram` 的标定值;本模型的值域更高(中位约 17),
/// 若直接用 17 做零点,高频与低频搭配会被压成同一档,实测并不更好。
/// 未命中时命中项按 0 计,于是恰好落在 `-WEIGHT × BASELINE`。
const BASELINE: f64 = 8.34;
/// 值 = `ln(频次) × 10000`。
const VALUE_SCALE: f64 = 10_000.0;
const FORMAT_PREFIX: &str = "Rime::Grammar/";

/// 掠影原生搭配表魔数: `LYCL` (Lueying Collocation) + 版本 1。
pub const NATIVE_MAGIC: &[u8; 4] = b"LYCL";
pub const NATIVE_VERSION: u32 = 1;
pub const NATIVE_HEADER_LEN: usize = 16;

/// 搭配表内核类型: 原生紧凑表或兼容的 Rime 双数组模型。
enum GramKind {
    Native { count: usize },
    Rime { da: usize, units: usize },
}

/// 只读打开的语法模型 / 搭配表。
pub struct Gram {
    mmap: Mmap,
    kind: GramKind,
}

impl Gram {
    /// 打开语法模型或原生搭配表;格式不符时返回错误,调用方按「无模型」降级。
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY:mmap 期间文件内容不得被改写。模型按版本目录整体替换
        // (写 `<name>.incoming` 后 rename),映射的 inode 内容不变。
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_mmap(mmap)
    }

    fn from_mmap(mmap: Mmap) -> io::Result<Self> {
        let bytes: &[u8] = &mmap;
        if bytes.starts_with(NATIVE_MAGIC) {
            if bytes.len() < NATIVE_HEADER_LEN {
                return Err(io::Error::other("掠影搭配表过短,缺少头部"));
            }
            let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            if version != NATIVE_VERSION {
                return Err(io::Error::other(format!("不支持的掠影搭配表版本: {version}")));
            }
            let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
            let expected_size = NATIVE_HEADER_LEN
                .checked_add(
                    count
                        .checked_mul(10)
                        .ok_or_else(|| io::Error::other("条数溢出"))?,
                )
                .ok_or_else(|| io::Error::other("长度溢出"))?;
            if bytes.len() != expected_size {
                return Err(io::Error::other(format!(
                    "掠影搭配表长度不符: 实际 {} 字节, 预期 {expected_size} 字节",
                    bytes.len()
                )));
            }
            return Ok(Self {
                mmap,
                kind: GramKind::Native { count },
            });
        }

        if bytes.len() < META {
            return Err(io::Error::other("语法模型过短,缺少 metadata"));
        }
        let format_end = bytes[..32].iter().position(|&b| b == 0).unwrap_or(32);
        let format = std::str::from_utf8(&bytes[..format_end]).unwrap_or("");
        if !format.starts_with(FORMAT_PREFIX) {
            return Err(io::Error::other(format!("不是 rime 语法模型或掠影搭配表: {format:?}")));
        }
        let units = u32::from_le_bytes(bytes[36..40].try_into().unwrap()) as usize;
        let ptr = i32::from_le_bytes(bytes[OFFSET_PTR_AT..OFFSET_PTR_AT + 4].try_into().unwrap());
        let da = (OFFSET_PTR_AT as i64 + i64::from(ptr)) as usize;
        // 这个等式本身就是格式判据:双数组紧贴 metadata 后的剩余字节,unit 4 字节。
        let Some(size) = units.checked_mul(UNIT) else {
            return Err(io::Error::other("语法模型 unit 数溢出"));
        };
        if da.checked_add(size) != Some(bytes.len()) {
            return Err(io::Error::other("语法模型双数组长度不符"));
        }
        Ok(Self {
            mmap,
            kind: GramKind::Rime { da, units },
        })
    }

    #[inline]
    fn rime_da_units(&self) -> Option<(usize, usize)> {
        match self.kind {
            GramKind::Rime { da, units } => Some((da, units)),
            GramKind::Native { .. } => None,
        }
    }

    #[inline]
    fn rime_units(&self) -> usize {
        match self.kind {
            GramKind::Rime { units, .. } => units,
            GramKind::Native { .. } => 0,
        }
    }

    fn unit(&self, i: usize) -> Option<u32> {
        let (da, _) = self.rime_da_units()?;
        let at = da.checked_add(i.checked_mul(UNIT)?)?;
        let b: [u8; 4] = self.mmap.get(at..at + UNIT)?.try_into().ok()?;
        Some(u32::from_le_bytes(b))
    }

    /// 沿编码串走到底,返回节点下标;任一步失配返回 `None`。
    fn traverse(&self, key: &[u8]) -> Option<usize> {
        let units = self.rime_units();
        let mut node = 0u32;
        let mut u = self.unit(0)?;
        for &b in key {
            node ^= offset_of(u) ^ u32::from(b);
            if node as usize >= units {
                return None;
            }
            u = self.unit(node as usize)?;
            if label_of(u) != u32::from(b) {
                return None;
            }
        }
        Some(node as usize)
    }

    /// 从节点出发,沿 `key` 扫过每个成词节点:命中搭配时直接算好带罚的分数,
    /// 只保留最高分,不分配临时 Vec。没有命中返回 0,方便调用方与句末分取 max。
    fn best_collocation(
        &self,
        key: &[u8],
        node: usize,
        context_chars: usize,
        whole_prefix: bool,
    ) -> f64 {
        let units = self.rime_units();
        if node >= units {
            return 0.0;
        }
        let mut best = 0.0f64;
        let mut id = node as u32;
        let Some(mut u) = self.unit(node) else {
            return 0.0;
        };
        let mut used = 0;
        for &b in key {
            used += 1;
            id ^= offset_of(u) ^ u32::from(b);
            if id as usize >= units {
                break;
            }
            let Some(next) = self.unit(id as usize) else {
                break;
            };
            u = next;
            if label_of(u) != u32::from(b) {
                break;
            }
            if has_leaf(u) {
                let leaf = id ^ offset_of(u);
                if let Some(v) = self.unit(leaf as usize)
                    && let Some(value) = value(v)
                {
                    let match_chars = encoded_chars_prefix(key, used);
                    let whole = whole_prefix && used == key.len();
                    let penalty = if context_chars + match_chars >= MIN_COLLOCATION_CHARS || whole {
                        0.0
                    } else {
                        WEAK_COLLOCATION_PENALTY
                    };
                    best = best.max(value + penalty);
                }
            }
        }
        best
    }

    /// 沿 `key` 找到第一个成词节点;句末 `$` 只取最短的命中,与 librime 一致。
    fn first_common_prefix(&self, key: &[u8], node: usize) -> Option<f64> {
        let units = self.rime_units();
        if node >= units {
            return None;
        }
        let mut id = node as u32;
        let mut u = self.unit(node)?;
        for &b in key {
            id ^= offset_of(u) ^ u32::from(b);
            if id as usize >= units {
                break;
            }
            let Some(next) = self.unit(id as usize) else {
                break;
            };
            u = next;
            if label_of(u) != u32::from(b) {
                break;
            }
            if has_leaf(u) {
                let leaf = id ^ offset_of(u);
                if let Some(v) = self.unit(leaf as usize)
                    && let Some(score) = value(v)
                {
                    return Some(score);
                }
            }
        }
        None
    }

    #[inline]
    fn native_key_at(&self, idx: usize) -> u64 {
        let offset = NATIVE_HEADER_LEN + idx * 8;
        u64::from_le_bytes(self.mmap[offset..offset + 8].try_into().unwrap())
    }

    #[inline]
    fn native_score_at(&self, idx: usize, count: usize) -> f64 {
        let offset = NATIVE_HEADER_LEN + count * 8 + idx * 2;
        let raw = i16::from_le_bytes(self.mmap[offset..offset + 2].try_into().unwrap());
        f64::from(raw) / 1000.0
    }

    /// 二分查找搭配键 `(left, right)` 的加分。
    pub fn native_find(&self, count: usize, left: &str, right: &str) -> Option<f64> {
        if count == 0 || left.is_empty() || right.is_empty() {
            return None;
        }
        let target = hash_pair(left, right);
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = (low + high) / 2;
            let k = self.native_key_at(mid);
            if k < target {
                low = mid + 1;
            } else if k > target {
                high = mid;
            } else {
                return Some(self.native_score_at(mid, count));
            }
        }
        None
    }

    fn score_native(&self, count: usize, context: &str, word: &str, is_rear: bool) -> f64 {
        let mut best = 0.0f64;
        // 1. 全词/全前文精准搭配
        if let Some(s) = self.native_find(count, context, word) {
            best = best.max(s);
        }
        // 2. 逐级缩短前文 (例如 "非常喜欢" -> 查 "喜欢"、"欢")
        let mut char_indices = context.char_indices();
        let _ = char_indices.next();
        for (byte_pos, _) in char_indices {
            let sub_ctx = &context[byte_pos..];
            if let Some(s) = self.native_find(count, sub_ctx, word) {
                best = best.max(s);
            }
        }
        // 3. 句末搭配
        if is_rear && let Some(s) = self.native_find(count, word, "$") {
            best = best.max(s);
        }
        best
    }

    fn score_rime(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        let query_chars = QUERY_CHARS.min(MAX_CHARS);
        let ctx = encode_tail(context, query_chars);
        let word_key = encode_head(word, query_chars);
        let mut hit = 0.0f64;
        let mut context_chars = encoded_chars(&ctx);
        let mut at = 0;
        while at < ctx.len() {
            if let Some(node) = self.traverse(&ctx[at..]) {
                hit = hit.max(self.best_collocation(&word_key, node, context_chars, at == 0));
            }
            context_chars = context_chars.saturating_sub(1);
            at += char_len(ctx[at]);
        }
        if is_rear
            && word.chars().count() <= query_chars
            && let Some(node) = self.traverse(&word_key)
            && let Some(score) = self.first_common_prefix(b"$", node)
        {
            hit = hit.max(score + REAR_PENALTY);
        }
        WEIGHT * (hit - BASELINE)
    }

    /// 给「`context` 之后接 `word`」打分。
    /// 原生格式未命中给 0.0(中性),命中给 PMI 增益;
    /// 兼容的 Rime 模型保留原有固定未命中惩罚与命中浮动。
    ///
    /// 首次调用(前文为空)返回 0,让调用方把「句首」当常数省掉。
    pub fn score(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        if context.is_empty() {
            return 0.0;
        }
        match self.kind {
            GramKind::Native { count } => self.score_native(count, context, word, is_rear),
            GramKind::Rime { .. } => self.score_rime(context, word, is_rear),
        }
    }
}

/// FNV-1a 64-bit 哈希,用于原生搭配表键 `left\0right`。
#[inline]
pub fn hash_pair(left: &str, right: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &b in left.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash ^= 0;
    hash = hash.wrapping_mul(FNV_PRIME);
    for &b in right.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[inline]
fn label_of(u: u32) -> u32 {
    u & 0x8000_00ff
}

/// darts-clone:bit 8 表示该节点带叶子。
#[inline]
fn has_leaf(u: u32) -> bool {
    u >> 8 & 1 == 1
}

#[inline]
fn offset_of(u: u32) -> u32 {
    (u >> 10) << ((u & 0x200) >> 6)
}

/// 叶子值:低 31 位是定点对数;最高位只是 darts 单元标志,不是无效位。
fn value(u: u32) -> Option<f64> {
    let value = u & 0x7fff_ffff;
    (value != 0).then(|| f64::from(value) / VALUE_SCALE)
}

/// 复刻 `gram_encoding.cc` 的变长编码:CJK 主区压成 2 字节,ASCII 原样,
/// 其余走 1 字节头 + 7 位块。
fn encode(ch: char, out: &mut Vec<u8>) {
    let u = ch as u32;
    if u < 0x80 {
        out.push(if u == 0 { 0xe0 } else { u as u8 });
    } else if (0x4000..0xa000).contains(&u) {
        if u & 0xff == 0 {
            out.push(0xe1);
            out.push(((u >> 8) + 0x40) as u8);
        } else {
            out.push(((u >> 8) + 0x40) as u8);
            out.push((u & 0xff) as u8);
        }
    } else {
        let mut bits = 32i32;
        let mut v = u;
        while bits > 0 && v & 0xfe00_0000 == 0 {
            bits -= 7;
            v <<= 7;
        }
        let mut n = (bits + 6) / 7;
        out.push(0xe0 | n as u8);
        while n > 0 {
            n -= 1;
            out.push(((v >> 25) & 0x7f) as u8 | 0x80);
            v <<= 7;
        }
    }
}

fn encode_head(s: &str, max_chars: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(max_chars * 2);
    for ch in s.chars().take(max_chars) {
        encode(ch, &mut out);
    }
    out
}

fn encode_tail(s: &str, max_chars: usize) -> Vec<u8> {
    let mut tail: Vec<char> = s.chars().rev().take(max_chars).collect();
    tail.reverse();
    let mut out = Vec::with_capacity(max_chars * 2);
    for ch in tail {
        encode(ch, &mut out);
    }
    out
}

/// 编码串里的 Unicode 字符数。
fn encoded_chars(s: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < s.len() {
        i += char_len(s[i]);
        n += 1;
    }
    n
}

/// 编码串前 `upto` 字节覆盖的完整字符数(落在字符中间时向下取整)。
fn encoded_chars_prefix(s: &[u8], upto: usize) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < s.len() {
        let len = char_len(s[i]);
        if i + len > upto {
            break;
        }
        i += len;
        n += 1;
    }
    n
}

/// 编码串里第一个字符占几个字节。0 字节视为非法,返回剩余长度以免死循环。
fn char_len(first: u8) -> usize {
    if first & 0x80 == 0 {
        1
    } else if first & 0xf0 == 0xe0 {
        (first & 0x0f) as usize + 1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_pair_deterministic() {
        let h1 = hash_pair("人工", "智能");
        let h2 = hash_pair("人工", "智能");
        assert_eq!(h1, h2);
        assert_ne!(hash_pair("吃", "药"), hash_pair("人工", "智能"));
    }

    #[test]
    fn test_native_collocation_table() {
        let mut pairs = vec![
            ("人工", "智能", 2.5),
            ("吃", "药", 1.8),
            ("不错", "$", 0.9),
        ];
        pairs.sort_by_key(|&(l, r, _)| hash_pair(l, r));

        let count = pairs.len();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(NATIVE_MAGIC);
        bytes.extend_from_slice(&NATIVE_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        for &(l, r, _) in &pairs {
            bytes.extend_from_slice(&hash_pair(l, r).to_le_bytes());
        }
        for &(_, _, s) in &pairs {
            let val = (s * 1000.0) as i16;
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        let temp_dir = std::env::temp_dir();
        let test_path = temp_dir.join(format!("test_collocation_{}.bin", std::process::id()));
        std::fs::write(&test_path, &bytes).unwrap();

        let gram = Gram::open(&test_path).expect("open native collocation table");
        assert!((gram.score("人工", "智能", false) - 2.5).abs() < 1e-3);
        assert!((gram.score("吃", "药", false) - 1.8).abs() < 1e-3);
        assert!((gram.score("前面人工", "智能", false) - 2.5).abs() < 1e-3);
        assert_eq!(gram.score("陌生", "搭配", false), 0.0);
        assert!((gram.score("天气", "不错", true) - 0.9).abs() < 1e-3);
        assert_eq!(gram.score("天气", "不错", false), 0.0);

        let _ = std::fs::remove_file(test_path);
    }
}
