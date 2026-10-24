//! 词典:文本 / 二进制统一加载,按键排序与精确 / 签名 / 前缀查询。
//!
//! 文本行格式 `词<TAB>词频<TAB>拼音音节`,音节以 `'` 分隔(`ni'hao`)。
//! `signature` 把键折叠成"每音节首字母、zh/ch/sh 归并为 z/c/s"的简拼签名,
//! 简拼与混合输入经签名索引一次定位到连续区段,再由引擎逐音节过滤。
//!
//! 两种来源汇入同一个表示:二进制文件整体 mmap,记录/字符串池/签名表都只是
//! 该字节区间的视图;文本加载先在内存里物化成同样的布局再走同一条查询路径。
//! 因此启动只需映射,不解析、不分配 per-entry 的 String。

pub mod syllables;

use std::collections::HashSet;
use std::ops::{Deref, Range};
use std::path::Path;

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

/// 二进制格式魔数:`PYD1` + 版本 1。
const MAGIC: &[u8; 8] = b"PYD1\x00\x00\x00\x01";
/// 记录字节数:word 偏移/长度、key 偏移/长度(4×u32)+ 词频(u64)。
const REC: usize = 24;
/// 签名表条目:签名字节池偏移/长度(2×u32)+ 记录下标(u32)。
const SIG: usize = 12;
/// 头部长度:魔数 + 条数 + 三段长度。
const HEAD: usize = MAGIC.len() + 16;

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
}

/// 词的每个字符都要是 CJK 汉字(白霜数据里偶有符号词条,直接丢弃)。
fn is_hanzi(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            matches!(c as u32,
                0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
        })
}

/// 键的音节签名:每音节取首字母(zh/ch/sh 的首字母恰好就是折叠后的 z/c/s)。
pub fn signature(key: &str) -> String {
    key.split('\'')
        .filter_map(|s| s.as_bytes().first().map(|&b| b as char))
        .collect()
}

fn push_str(data: &mut Vec<u8>, s: &str) -> (u32, u32) {
    let off = data.len() as u32;
    data.extend_from_slice(s.as_bytes());
    (off, s.len() as u32)
}

/// 把已排序的 `(词, 键, 词频)` 物化成二进制布局。
fn pack(raw: &[(String, String, u64)]) -> Vec<u8> {
    let mut data: Vec<u8> = Vec::with_capacity(raw.len() * 32);
    let mut recs: Vec<u8> = Vec::with_capacity(raw.len() * REC);
    let mut sigs: Vec<(u32, u32, u32)> = Vec::with_capacity(raw.len());
    for (i, (word, key, freq)) in raw.iter().enumerate() {
        let (wo, wl) = push_str(&mut data, word);
        let (ko, kl) = push_str(&mut data, key);
        for n in [wo, wl, ko, kl] {
            recs.extend_from_slice(&n.to_le_bytes());
        }
        recs.extend_from_slice(&freq.to_le_bytes());
        let sig = signature(key);
        let (so, sl) = push_str(&mut data, &sig);
        sigs.push((so, sl, i as u32));
    }
    // 签名按字典序排,查询才能二分。
    sigs.sort_by(|a, b| {
        let sa = &data[a.0 as usize..(a.0 + a.1) as usize];
        let sb = &data[b.0 as usize..(b.0 + b.1) as usize];
        sa.cmp(sb).then(a.2.cmp(&b.2))
    });
    let mut out = Vec::with_capacity(HEAD + recs.len() + data.len() + sigs.len() * SIG);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(sigs.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&recs);
    out.extend_from_slice(&data);
    for (so, sl, idx) in sigs {
        out.extend_from_slice(&so.to_le_bytes());
        out.extend_from_slice(&sl.to_le_bytes());
        out.extend_from_slice(&idx.to_le_bytes());
    }
    out
}

impl Dictionary {
    /// 解析文本词典。格式或音节不合法的行跳过并计数;同 (词, 键) 保留最大词频。
    pub fn from_text(text: &str) -> (Dictionary, usize) {
        let mut raw: Vec<(String, String, u64)> = Vec::new();
        let mut bad = 0usize;
        let mut seen: HashSet<(String, String)> = HashSet::new();
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
            if !is_hanzi(word)
                || key.is_empty()
                || word.chars().count() != key.split('\'').count()
                || !key.split('\'').all(syllables::is_syllable)
            {
                bad += 1;
                continue;
            }
            if !seen.insert((word.to_string(), key.to_string())) {
                continue;
            }
            raw.push((word.to_string(), key.to_string(), freq));
        }
        raw.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
        (Self::from_bytes(Backing::Owned(pack(&raw))), bad)
    }

    /// 打开词典:二进制则 mmap(零解析),否则按 UTF-8 文本解析。
    pub fn from_path(path: &Path) -> std::io::Result<(Dictionary, usize)> {
        let file = std::fs::File::open(path)?;
        // 先看魔数决定走哪条路;映射失败(空文件等)退回读入内存。
        let bytes = std::fs::read(path)?;
        if bytes.len() >= MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC {
            let mapped = unsafe { Mmap::map(&file) }.ok();
            return match mapped {
                Some(m) if m.len() == bytes.len() => Ok((Self::from_bytes(Backing::Mapped(m)), 0)),
                _ => Ok((Self::from_bytes(Backing::Owned(bytes)), 0)),
            };
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| std::io::Error::other("词典既非二进制也非 UTF-8 文本"))?;
        Ok(Self::from_text(&text))
    }

    /// 从二进制布局建立视图。字段越界时按空词典处理(坏文件不拖垮输入法)。
    fn from_bytes(backing: Backing) -> Dictionary {
        let bytes: &[u8] = &backing;
        let empty = Dictionary {
            backing,
            recs: 0..0,
            data: 0..0,
            sigs: 0..0,
            count: 0,
        };
        if bytes.len() < HEAD || &bytes[..MAGIC.len()] != MAGIC {
            return empty;
        }
        let n = |i: usize| {
            u32::from_le_bytes(bytes[HEAD - 16 + i * 4..HEAD - 12 + i * 4].try_into().unwrap())
                as usize
        };
        let (count, data_len, sig_len) = (n(0), n(1), n(2));
        let recs_at = HEAD;
        let data_at = recs_at + count * REC;
        let sigs_at = data_at + data_len;
        let end = sigs_at + sig_len * SIG;
        if end > bytes.len() {
            return empty;
        }
        Dictionary {
            backing,
            recs: recs_at..data_at,
            data: data_at..sigs_at,
            sigs: sigs_at..end,
            count,
        }
    }

    /// 物化二进制格式(写盘用)。二进制载入的词典再导出仍是等价字节。
    pub fn to_binary(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.backing.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.count as u32).to_le_bytes());
        out.extend_from_slice(&((self.data.end - self.data.start) as u32).to_le_bytes());
        out.extend_from_slice(&((self.sigs.end - self.sigs.start) / SIG as usize).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&self.backing[self.recs.clone()]);
        out.extend_from_slice(&self.backing[self.data.clone()]);
        out.extend_from_slice(&self.backing[self.sigs.clone()]);
        out
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

    fn rec(&self, i: usize) -> &[u8] {
        let at = self.recs.start + i * REC;
        &self.backing[at..at + REC]
    }

    fn entry(&self, i: usize) -> Entry<'_> {
        let r = self.rec(i);
        let u = |n: usize| {
            u32::from_le_bytes(r[n * 4..n * 4 + 4].try_into().unwrap()) as usize
        };
        let (wo, wl, ko, kl) = (u(0), u(1), u(2), u(3));
        let text = |off: usize, len: usize| {
            let at = self.data.start + off;
            std::str::from_utf8(&self.backing[at..at + len]).unwrap_or("")
        };
        Entry {
            word: text(wo, wl),
            key: text(ko, kl),
            freq: u64::from_le_bytes(r[16..24].try_into().unwrap()),
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

    fn sig_at(&self, i: usize) -> (&str, u32) {
        let at = self.sigs.start + i * SIG;
        let r = &self.backing[at..at + SIG];
        let off = u32::from_le_bytes(r[0..4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(r[4..8].try_into().unwrap()) as usize;
        let idx = u32::from_le_bytes(r[8..12].try_into().unwrap());
        let at = self.data.start + off;
        (
            std::str::from_utf8(&self.backing[at..at + len]).unwrap_or(""),
            idx,
        )
    }

    /// 签名查询:返回该签名的连续区段(调用方再逐音节过滤),最多 `max_visits` 条。
    pub fn lookup_sig(&self, sig: &str, max_visits: usize) -> Vec<Entry<'_>> {
        let n = (self.sigs.end - self.sigs.start) / SIG;
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.sig_at(mid).0 < sig {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut out = Vec::new();
        for i in lo..n {
            let (s, idx) = self.sig_at(i);
            if s != sig || out.len() >= max_visits {
                break;
            }
            out.push(self.entry(idx as usize));
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
}
