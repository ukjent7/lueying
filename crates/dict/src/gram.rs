//! librime-octagram `.gram` 语言模型:mmap 零解析 + darts-clone 双数组查询。
//!
//! 格式(与 librime `gram_db.cc` 一致):44 字节 metadata(`format[32]`、
//! `checksum`、`double_array_size`、`OffsetPtr`)后紧跟 4 字节 unit 的双数组。
//! 每个键是「前文尾部 ≤N 字 + 当前词首部 ≤N 字」的字级编码串,值是该搭配的
//! `ln(频次) × 10000`。查询只做 `traverse` + `commonPrefixSearch`,所以文件可以
//! 直接映射、不反序列化。
//!
//! 打分复刻 librime-octagram 的 `Query`:命中搭配的值加上搭配罚,未命中给未搭配
//! 罚,句末再查一次 `$`。默认参数与万象拼音方案一致;模型用
//! `amzxyz/rime-wanxiang-data` 的 LTS 版本。
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
//! 单次查询最多取几个前缀匹配,对齐 `GramDb::kMaxResults`。
const MAX_RESULTS: usize = 8;
/// 一次编码最多几个 Unicode 字符。
const MAX_CHARS: usize = 8;
/// 查搭配时前文与词各取几个字(librime 的 `collocation_max_length - 1`)。
const QUERY_CHARS: usize = 5;
/// 调用方提供的前文尾长。模型键是「前文尾 3 字 + 词首 3 字」,取 3 字最贴合。
pub const CONTEXT_CHARS: usize = 3;
/// librime-octagram 的搭配打分参数(与万象方案一致)。
const MIN_COLLOCATION_CHARS: usize = 2;
const COLLOCATION_PENALTY: f64 = -14.0;
const NON_COLLOCATION_PENALTY: f64 = -6.0;
const WEAK_COLLOCATION_PENALTY: f64 = -100.0;
const REAR_PENALTY: f64 = -8.0;
/// 值 = `ln(频次) × 10000`。
const VALUE_SCALE: f64 = 10_000.0;
const FORMAT_PREFIX: &str = "Rime::Grammar/";

/// 只读打开的语法模型。
pub struct Gram {
    mmap: Mmap,
    /// 双数组起点(文件内字节偏移)。
    da: usize,
    /// 双数组 unit 数。
    units: usize,
}

impl Gram {
    /// 打开 `.gram`;格式不符时返回错误,调用方按「无模型」降级。
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY:mmap 期间文件内容不得被改写。模型按版本目录整体替换
        // (写 `<name>.incoming` 后 rename),映射的 inode 内容不变。
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_mmap(mmap)
    }

    fn from_mmap(mmap: Mmap) -> io::Result<Self> {
        let bytes: &[u8] = &mmap;
        if bytes.len() < META {
            return Err(io::Error::other("语法模型过短,缺少 metadata"));
        }
        let format_end = bytes[..32].iter().position(|&b| b == 0).unwrap_or(32);
        let format = std::str::from_utf8(&bytes[..format_end]).unwrap_or("");
        if !format.starts_with(FORMAT_PREFIX) {
            return Err(io::Error::other(format!("不是 rime 语法模型: {format:?}")));
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
        Ok(Self { mmap, da, units })
    }

    fn unit(&self, i: usize) -> Option<u32> {
        let at = self.da.checked_add(i.checked_mul(UNIT)?)?;
        let b: [u8; 4] = self.mmap.get(at..at + UNIT)?.try_into().ok()?;
        Some(u32::from_le_bytes(b))
    }

    /// 沿编码串走到底,返回节点下标;任一步失配返回 `None`。
    fn traverse(&self, key: &[u8]) -> Option<usize> {
        let mut node = 0u32;
        let mut u = self.unit(0)?;
        for &b in key {
            node ^= offset_of(u) ^ u32::from(b);
            if node as usize >= self.units {
                return None;
            }
            u = self.unit(node as usize)?;
            if label_of(u) != u32::from(b) {
                return None;
            }
        }
        Some(node as usize)
    }

    /// 从节点出发,沿 `key` 收集每个成词节点的值与已匹配的编码字节数。
    fn common_prefix(&self, key: &[u8], node: usize) -> Vec<(f64, usize)> {
        let mut out = Vec::new();
        if node >= self.units {
            return out;
        }
        let mut id = node as u32;
        let mut u = self.unit(node).unwrap_or(0);
        let mut used = 0;
        for &b in key {
            used += 1;
            id ^= offset_of(u) ^ u32::from(b);
            if id as usize >= self.units {
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
                    && out.len() < MAX_RESULTS
                {
                    out.push((score, used));
                }
            }
        }
        out
    }

    /// 给「`context` 之后接 `word`」打分,复刻 librime-octagram 的 `Query`:
    /// 取所有前文后缀与词前缀组合里的最高分,命中的值加上搭配罚;
    /// 词尾再查一次 `$` 的句末搭配。无命中返回未搭配罚。
    ///
    /// 这个分数直接加在词的对数权重上;首次调用(前文为空)返回 0,
    /// 让调用方把「句首」当常数省掉。
    pub fn score(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        if context.is_empty() {
            return 0.0;
        }
        let ctx = encode_tail(context, QUERY_CHARS.min(MAX_CHARS));
        let word_key = encode_head(word, QUERY_CHARS.min(MAX_CHARS));
        let mut result = NON_COLLOCATION_PENALTY;
        let mut context_chars = encoded_chars(&ctx);
        let mut at = 0;
        while at < ctx.len() {
            if let Some(node) = self.traverse(&ctx[at..]) {
                for (value, matched) in self.common_prefix(&word_key, node) {
                    let match_chars = encoded_chars_prefix(&word_key, matched);
                    let whole = at == 0 && matched == word_key.len();
                    let penalty = if context_chars + match_chars >= MIN_COLLOCATION_CHARS || whole {
                        COLLOCATION_PENALTY
                    } else {
                        WEAK_COLLOCATION_PENALTY
                    };
                    result = result.max(value + penalty);
                }
            }
            context_chars = context_chars.saturating_sub(1);
            at += char_len(ctx[at]);
        }
        if is_rear && word.chars().count() <= QUERY_CHARS {
            let key = encode_head(word, QUERY_CHARS.min(MAX_CHARS));
            if let Some(node) = self.traverse(&key)
                && let Some((value, _)) = self.common_prefix(b"$", node).first()
            {
                result = result.max(*value + REAR_PENALTY);
            }
        }
        result
    }
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

/// 叶子值:低 31 位是定点对数;最高位为 1 表示无效(librime 用 -1 表示未命中)。
fn value(u: u32) -> Option<f64> {
    (u & 0x8000_0000 == 0).then(|| f64::from(u) / VALUE_SCALE)
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
