//! 词典:文本 / 二进制统一加载,按键排序与精确 / 签名 / 前缀查询。
//!
//! 文本行格式 `词<TAB>词频<TAB>拼音音节`,音节以 `'` 分隔(`ni'hao`)。
//! `signature` 把键折叠成"每音节首字母、zh/ch/sh 归并为 z/c/s"的简拼签名,
//! 简拼与混合输入经签名索引一次定位到连续区段,再由引擎逐音节过滤。
//!
//! 两种来源汇入同一个表示:二进制文件整体 mmap,记录/字符串池/签名表都只是
//! 该字节区间的视图;文本加载先在内存里物化成同样的布局再走同一条查询路径。
//! 因此启动只需映射,不解析、不分配 per-entry 的 String。

pub mod gram;
pub mod syllables;

use std::collections::HashMap;
use std::io::Read;
use std::ops::{Deref, Range};
use std::path::Path;
use std::sync::OnceLock;

pub use gram::Gram;
use memmap2::Mmap;
pub use syllables::Fuzzy;

/// 一条词条(借用词典内部的字节区间)。
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub word: &'a str,
    /// 音节键,`'` 分隔。
    pub key: &'a str,
    pub freq: u64,
}

/// 二进制格式魔数:`PYD2` + 版本 1。
const MAGIC: &[u8; 8] = b"PYD2\x00\x00\x00\x01";
/// 记录字节数:word 偏移、key 偏移、词频(3×u32)+ word 长度、key 长度(2×u8)。
/// 记录按键排序,同键共用字符串池里的一份键。
const REC: usize = 14;
/// 签名表条目:记录下标(u32)。签名由键现算,不占字符串池。
const SIG: usize = 4;
/// 头部长度:魔数 + 条数、字符串池长(各 4 字节)。
const HEAD: usize = MAGIC.len() + 8;

/// 词典字节的来源:映射文件或内存。
enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => m,
            Backing::Owned(v) => v,
        }
    }
}

/// 已排序的词典:记录按 (key, freq 逆序, word) 排;签名表按签名字节排。
pub struct Dictionary {
    backing: Backing,
    /// 记录表区间,每个 `REC` 字节一条。
    recs: Range<usize>,
    /// 字符串池区间:词、键、签名都放这里。
    data: Range<usize>,
    /// 签名表区间,每个 `SIG` 字节一条。
    sigs: Range<usize>,
    count: usize,
    /// 词频总和(首次整句解码时算一次)。
    total: OnceLock<u64>,
}

/// 词的每个字符都要是 CJK 汉字(符号词条直接丢弃)。
fn is_hanzi(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            matches!(c as u32,
                0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
                | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x30000..=0x323AF)
        })
}

/// 键的音节签名字节:每音节取首字母(zh/ch/sh 的首字母恰好就是折叠后的 z/c/s)。
fn sig_bytes(key: &str) -> impl Iterator<Item = u8> + '_ {
    key.split('\'').filter_map(|s| s.as_bytes().first().copied())
}

/// 键的音节签名。
pub fn signature(key: &str) -> String {
    sig_bytes(key).map(char::from).collect()
}

fn push_str(data: &mut Vec<u8>, s: &str) -> u32 {
    let off = data.len() as u32;
    data.extend_from_slice(s.as_bytes());
    off
}

/// 把已排序的 `(词, 键, 词频)` 物化成二进制布局。
fn pack(raw: &[(String, String, u64)]) -> Vec<u8> {
    let mut data: Vec<u8> = Vec::with_capacity(raw.len() * 20);
    let mut recs: Vec<u8> = Vec::with_capacity(raw.len() * REC);
    let mut last: Option<(&str, u32)> = None;
    for (word, key, freq) in raw {
        let wo = push_str(&mut data, word);
        // 记录已按键排序,相邻同键共用一份字符串。
        let ko = match last {
            Some((prev, off)) if prev == key => off,
            _ => push_str(&mut data, key),
        };
        last = Some((key.as_str(), ko));
        recs.extend_from_slice(&wo.to_le_bytes());
        recs.extend_from_slice(&ko.to_le_bytes());
        recs.extend_from_slice(&u32::try_from(*freq).unwrap_or(u32::MAX).to_le_bytes());
        recs.push(word.len() as u8);
        recs.push(key.len() as u8);
    }
    // 签名按字典序排,查询才能二分。
    let mut sigs: Vec<u32> = (0..raw.len() as u32).collect();
    sigs.sort_by(|&a, &b| {
        sig_bytes(&raw[a as usize].1)
            .cmp(sig_bytes(&raw[b as usize].1))
            .then(a.cmp(&b))
    });
    let mut out = Vec::with_capacity(HEAD + recs.len() + data.len() + sigs.len() * SIG);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&recs);
    out.extend_from_slice(&data);
    for idx in sigs {
        out.extend_from_slice(&idx.to_le_bytes());
    }
    out
}

impl Dictionary {
    /// 解析文本词典。格式或音节不合法的行跳过并计数;同 (词, 键) 保留最大词频。
    pub fn from_text(text: &str) -> (Dictionary, usize) {
        let mut raw: Vec<(String, String, u64)> = Vec::new();
        let mut bad = 0usize;
        let mut seen: HashMap<(String, String), usize> = HashMap::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(word), Some(freq), Some(key)) =
                (parts.next(), parts.next().map(|s| s.trim()), parts.next())
            else {
                bad += 1;
                continue;
            };
            let Ok(freq) = freq.parse::<u64>() else {
                bad += 1;
                continue;
            };
            let word = word.trim();
            // 词库统一存规范形,查询用 `lue`/`nue` 也能命中 `lve`/`nve` 词条。
            let key = key
                .split('\'')
                .map(syllables::canonical)
                .collect::<Vec<_>>()
                .join("'");
            if !is_hanzi(word)
                || key.is_empty()
                || word.len() > u8::MAX as usize
                || key.len() > u8::MAX as usize
                || word.chars().count() != key.split('\'').count()
                || !key.split('\'').all(syllables::is_syllable)
            {
                bad += 1;
                continue;
            }
            let ident = (word.to_string(), key.clone());
            if let Some(&at) = seen.get(&ident) {
                raw[at].2 = raw[at].2.max(freq);
                continue;
            }
            seen.insert(ident, raw.len());
            raw.push((word.to_string(), key, freq));
        }
        raw.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
        // pack 产出的字节一定合法,unwrap 安全。
        let dict = Self::from_bytes(Backing::Owned(pack(&raw))).unwrap();
        (dict, bad)
    }

    /// 打开词典:二进制则 mmap(零解析),否则按 UTF-8 文本解析。
    pub fn from_path(path: &Path) -> std::io::Result<(Dictionary, usize)> {
        let file = std::fs::File::open(path)?;
        // 只读魔数判断格式;别把几十 MB 的二进制词典先整份读进内存,否则
        // mmap 的启动/峰值内存优势会被抵消。
        let mut magic = [0u8; MAGIC.len()];
        let mut probe = &file;
        let count = probe.read(&mut magic)?;
        // 映射失败(空文件、权限等)时走下面的整份读入。
        if count == MAGIC.len()
            && magic == *MAGIC
            && let Ok(mapped) = unsafe { Mmap::map(&file) }
        {
            return Self::from_bytes(Backing::Mapped(mapped))
                .map(|d| (d, 0))
                .ok_or_else(|| std::io::Error::other("二进制词典损坏"));
        }
        let bytes = std::fs::read(path)?;
        if bytes.len() >= MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC {
            return Self::from_bytes(Backing::Owned(bytes))
                .map(|d| (d, 0))
                .ok_or_else(|| std::io::Error::other("二进制词典损坏"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| std::io::Error::other("词典既非二进制也非 UTF-8 文本"))?;
        Ok(Self::from_text(&text))
    }

    /// 从二进制字节建立词典(内存来源)。魔数/长度不符返回 None。
    pub fn from_binary(bytes: &[u8]) -> Option<Dictionary> {
        Self::from_bytes(Backing::Owned(bytes.to_vec()))
    }

    /// 从二进制布局建立视图。魔数或长度不符返回 None;空词典是合法结果。
    fn from_bytes(backing: Backing) -> Option<Dictionary> {
        // 先在借用期内算好区间,再移交所有权(否则 bytes 的借用挡住 move)。
        let layout = {
            let bytes: &[u8] = &backing;
            if bytes.len() < HEAD || &bytes[..MAGIC.len()] != MAGIC {
                None
            } else {
                // 头部两个 u32 紧随魔数:条数、字符串池长。签名表与记录等长。
                let n = |i: usize| {
                    let at = MAGIC.len() + i * 4;
                    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
                };
                let (count, data_len) = (n(0), n(1));
                let recs_at = HEAD;
                // 32 位宿主上 count 来自文件,乘法可能回绕;损坏的头
                // 必须整份拒绝,不能靠回绕后的区间碰巧通过长度检查。
                let layout = || {
                    let data_at = HEAD.checked_add(count.checked_mul(REC)?)?;
                    let sigs_at = data_at.checked_add(data_len)?;
                    let end = sigs_at.checked_add(count.checked_mul(SIG)?)?;
                    (end <= bytes.len()).then_some((
                        count,
                        recs_at..data_at,
                        data_at..sigs_at,
                        sigs_at..end,
                    ))
                };
                layout()
            }
        };
        let (count, recs, data, sigs) = layout?;
        Some(Dictionary {
            backing,
            recs,
            data,
            sigs,
            count,
            total: OnceLock::new(),
        })
    }

    /// 物化二进制格式(写盘用)。从词条重新打包而非复制内部字节,
    /// 输出与 `pack` 严格同构,因此对任何来源的词典都幂等。
    pub fn to_binary(&self) -> Vec<u8> {
        let raw: Vec<(String, String, u64)> = self
            .iter()
            .map(|e| (e.word.to_string(), e.key.to_string(), e.freq))
            .collect();
        pack(&raw)
    }

    /// 内置迷你词库(离线开发与 E2E 用)。
    pub fn builtin() -> Dictionary {
        let (dict, bad) = Self::from_text(include_str!("../../../dicts/mini.txt"));
        assert_eq!(bad, 0, "内置词库不应有坏行");
        dict
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 按记录顺序遍历全部词条(校验、导出用)。
    pub fn iter(&self) -> impl Iterator<Item = Entry<'_>> {
        (0..self.count).map(|i| self.entry(i))
    }

    fn rec(&self, i: usize) -> &[u8] {
        let at = self.recs.start + i * REC;
        &self.backing[at..at + REC]
    }

    fn entry(&self, i: usize) -> Entry<'_> {
        let r = self.rec(i);
        let u = |n: usize| u32::from_le_bytes(r[n * 4..n * 4 + 4].try_into().unwrap());
        let (wo, ko, freq) = (u(0) as usize, u(1) as usize, u(2));
        let (wl, kl) = (r[12] as usize, r[13] as usize);
        // 区间必须落在字符串池内:越界说明文件损坏,返回空串而非静默读到别处。
        let text = |off: usize, len: usize| {
            let Some(at) = self.data.start.checked_add(off) else {
                return "";
            };
            let Some(end) = at.checked_add(len) else {
                return "";
            };
            if end > self.data.end {
                return "";
            }
            std::str::from_utf8(&self.backing[at..end]).unwrap_or("")
        };
        Entry {
            word: text(wo, wl),
            key: text(ko, kl),
            freq: u64::from(freq),
        }
    }

    /// 第 i 条记录的音节键(签名比较用,复用 entry 的切片逻辑)。
    fn key_at(&self, i: usize) -> &str {
        self.entry(i).key
    }

    /// 精确键查询(全拼音节键):二分找 key 的连续区段。
    pub fn lookup_exact(&self, key: &str) -> Vec<Entry<'_>> {
        let start = self.partition_key(key);
        let mut out = Vec::new();
        for i in start..self.count {
            let e = self.entry(i);
            if e.key != key {
                break;
            }
            out.push(e);
        }
        out
    }

    /// 按 key 二分:首个 `key_at(i) >= key` 的下标。
    fn partition_key(&self, key: &str) -> usize {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.key_at(mid) < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// 签名表第 i 项指向的记录下标。
    fn sig_at(&self, i: usize) -> usize {
        let at = self.sigs.start + i * SIG;
        u32::from_le_bytes(self.backing[at..at + SIG].try_into().unwrap()) as usize
    }

    /// 按签名二分:首个签名 `>= sig` 的签名表位置。
    fn partition_sig(&self, sig: &str) -> usize {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let idx = self.sig_at(mid);
            if idx < self.count && sig_bytes(self.key_at(idx)).lt(sig.bytes()) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// 签名查询:返回该签名的连续区段(调用方再逐音节过滤),最多 `max_visits` 条。
    pub fn lookup_sig(&self, sig: &str, max_visits: usize) -> Vec<Entry<'_>> {
        let mut out = Vec::new();
        for i in self.partition_sig(sig)..self.count {
            let idx = self.sig_at(i);
            if idx >= self.count || out.len() >= max_visits {
                break;
            }
            let entry = self.entry(idx);
            if !sig_bytes(entry.key).eq(sig.bytes()) {
                break;
            }
            out.push(entry);
        }
        out
    }

    /// 键前缀查询(前缀以 `'` 结尾保证音节边界),最多 `max_visits` 条。
    pub fn lookup_prefix(&self, prefix: &str, max_visits: usize) -> Vec<Entry<'_>> {
        let start = self.partition_key(prefix);
        let mut out = Vec::new();
        for i in start..self.count {
            let e = self.entry(i);
            if !e.key.starts_with(prefix) || out.len() >= max_visits {
                break;
            }
            out.push(e);
        }
        out
    }

    /// 是否存在以 `prefix` 开头的键(整句建图的剪枝用,不取词条)。
    pub fn has_key_prefix(&self, prefix: &str) -> bool {
        let i = self.partition_key(prefix);
        i < self.count && self.key_at(i).starts_with(prefix)
    }

    /// 是否存在以 `prefix` 开头的签名(简拼混合输入建图剪枝用)。
    pub fn has_sig_prefix(&self, prefix: &str) -> bool {
        let at = self.partition_sig(prefix);
        at < self.count && {
            let idx = self.sig_at(at);
            idx < self.count
                && sig_bytes(self.key_at(idx))
                    .take(prefix.len())
                    .eq(prefix.bytes())
        }
    }

    /// 词频总和(每个词条至少按 1 计),用于把词频归一成对数概率。
    pub fn total_weight(&self) -> u64 {
        *self
            .total
            .get_or_init(|| self.iter().map(|e| e.freq.max(1)).sum())
    }
}
