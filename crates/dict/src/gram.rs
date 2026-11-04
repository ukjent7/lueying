//! 掠影搭配表 `LYCL` v2:mmap 零解析,按词对指纹二分查 PMI 分值。
//!
//! 文件布局(小端):16 字节头(`LYCL` + 版本 u32 + 条数 u32 + 保留 u32),随后是按
//! 指纹升序排列的 `count` 个 u32 指纹,再是同序的 `count` 个 u8 分值。指纹是词对
//! `left\0right` 的 FNV-1a 64 位哈希取高 32 位;分值 `q` 表示 `q / 42.5`,即 0~6
//! 线性量化到 0~255。构建端是 lueying-dict 仓库的 `scripts/collocate.py`。
//!
//! 打分:前文逐级缩短各查一次、句末再查 `$`,取最高分。未收录给 0(中性),命中给
//! 正的 PMI 增益。

use std::fs::File;
use std::io;
use std::path::Path;

use memmap2::Mmap;

/// 魔数 `LYCL`(Lueying Collocation)。
const MAGIC: &[u8; 4] = b"LYCL";
/// 格式版本。v1(u64 哈希加 i16 分值)已废弃,读到即拒绝。
const VERSION: u32 = 2;
/// 头部 16 字节:魔数、版本、条数、保留字段各 4 字节。
const HEADER_LEN: usize = 16;
/// 每条记录 5 字节:u32 指纹加 u8 分值。
const RECORD_LEN: usize = 5;
/// 分值量化尺度:0~6 映射到 0~255。
const SCORE_SCALE: f64 = 255.0 / 6.0;
/// 调用方取的前文尾长(字数)。构建端只收左词不超过 3 字的词对(`MAX_LEFT`),
/// 更长的前文只会逐级缩短后再查,多取无益。
pub const CONTEXT_CHARS: usize = 3;

/// 只读打开的搭配表。
pub struct Gram {
    mmap: Mmap,
    count: usize,
}

impl Gram {
    /// 打开搭配表;魔数、版本或长度不符时返回错误,调用方按「无搭配表」降级。
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY:mmap 期间文件内容不得被改写。模型按版本目录整体替换
        // (写 `<name>.incoming` 后 rename),映射的 inode 内容不变。
        let mmap = unsafe { Mmap::map(&file)? };
        let bytes: &[u8] = &mmap;
        if bytes.len() < HEADER_LEN || !bytes.starts_with(MAGIC) {
            return Err(io::Error::other("不是掠影搭配表"));
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != VERSION {
            return Err(io::Error::other(format!(
                "不支持的掠影搭配表版本: {version}"
            )));
        }
        let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let expected = count
            .checked_mul(RECORD_LEN)
            .and_then(|n| n.checked_add(HEADER_LEN));
        if expected != Some(bytes.len()) {
            return Err(io::Error::other(format!(
                "掠影搭配表长度不符: 实际 {} 字节, 条数 {count}",
                bytes.len()
            )));
        }
        Ok(Self { mmap, count })
    }

    /// 第 `idx` 条的指纹。
    fn fingerprint_at(&self, idx: usize) -> u32 {
        let at = HEADER_LEN + idx * 4;
        u32::from_le_bytes(self.mmap[at..at + 4].try_into().unwrap())
    }

    /// 第 `idx` 条的分值。分值区紧跟在全部指纹之后。
    fn score_at(&self, idx: usize) -> f64 {
        f64::from(self.mmap[HEADER_LEN + self.count * 4 + idx]) / SCORE_SCALE
    }

    /// 二分查找词对 `(left, right)` 的分值;未收录返回 `None`。
    fn find(&self, left: &str, right: &str) -> Option<f64> {
        let target = fingerprint(left, right);
        // 下界二分:第一个指纹不小于目标的位置。
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.fingerprint_at(mid) < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo < self.count && self.fingerprint_at(lo) == target).then(|| self.score_at(lo))
    }

    /// 给「`context` 之后接 `word`」打分;`is_rear` 表示 `word` 是句末。
    /// 前文为空返回 0,让调用方把句首当常数省掉。
    pub fn score(&self, context: &str, word: &str, is_rear: bool) -> f64 {
        if context.is_empty() {
            return 0.0;
        }
        // 前文逐级缩短各查一次:"非常喜欢" 依次查 "非常喜欢"、"常喜欢"、"喜欢"、"欢"。
        let shorter = context.char_indices().map(|(i, _)| &context[i..]);
        let rear = is_rear.then(|| self.find(word, "$")).flatten();
        shorter
            .filter_map(|ctx| self.find(ctx, word))
            .chain(rear)
            .fold(0.0, f64::max)
    }
}

/// 词对指纹:`left\0right` 的 FNV-1a 64 位哈希取高 32 位,与构建端一致。
fn fingerprint(left: &str, right: &str) -> u32 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &b in left.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    // 分隔符 `\0` 异或为 0,只需再乘一次素数。
    hash = hash.wrapping_mul(FNV_PRIME);
    for &b in right.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 量化误差不超过半格,测试容差取一整格。
    const STEP: f64 = 1.0 / SCORE_SCALE;

    #[test]
    fn test_fingerprint_matches_builder() {
        // 与 scripts/collocate.py 的 `fingerprint("人工\0智能")` 取值一致。
        assert_eq!(fingerprint("人工", "智能"), 0x4d7c_8ed6);
        assert_ne!(fingerprint("吃", "药"), fingerprint("人工", "智能"));
    }

    #[test]
    fn test_collocation_table() {
        let mut pairs = vec![("人工", "智能", 2.5), ("吃", "药", 1.8), ("不错", "$", 0.9)];
        pairs.sort_by_key(|&(l, r, _)| fingerprint(l, r));

        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for &(l, r, _) in &pairs {
            bytes.extend_from_slice(&fingerprint(l, r).to_le_bytes());
        }
        for &(_, _, s) in &pairs {
            bytes.push((s * SCORE_SCALE).round() as u8);
        }

        let temp_dir = std::env::temp_dir();
        let test_path = temp_dir.join(format!("test_collocation_{}.bin", std::process::id()));
        std::fs::write(&test_path, &bytes).unwrap();

        let gram = Gram::open(&test_path).expect("open collocation table");
        assert!((gram.score("人工", "智能", false) - 2.5).abs() < STEP);
        assert!((gram.score("吃", "药", false) - 1.8).abs() < STEP);
        assert!((gram.score("前面人工", "智能", false) - 2.5).abs() < STEP);
        assert_eq!(gram.score("陌生", "搭配", false), 0.0);
        assert!((gram.score("天气", "不错", true) - 0.9).abs() < STEP);
        assert_eq!(gram.score("天气", "不错", false), 0.0);

        let _ = std::fs::remove_file(test_path);
    }
}
