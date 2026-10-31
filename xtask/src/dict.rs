//! 白霜拼音词典转换:五表解析、常用词提频、异读补录与 TSV/二进制物化。
//!
//! `main` 的 `#[path]` 子模块(src 保持扁平);根上只留子命令分发。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use lueying_dict::syllables::is_syllable;

pub(crate) struct Args {
    frost: PathBuf,
    out: PathBuf,
    /// 另行物化的二进制词典(运行期 mmap 零解析加载)。
    bin: Option<PathBuf>,
}

pub(crate) fn parse_dict(it: &mut impl Iterator<Item = String>) -> Args {
    let mut frost = None;
    let mut out = None;
    let mut bin = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frost" => frost = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--bin" => bin = it.next().map(PathBuf::from),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    Args {
        frost: frost.expect("--frost 需要 <白霜 cn_dicts 目录>"),
        out: out.expect("--out 需要 <输出文件>"),
        bin,
    }
}

/// 发布目录的必需文件。DLL 文件名随 Cargo 包名生成,两种拼写都接受。
const FILES: &[&str] = &[
    "base.dict.yaml",
    "8105.dict.yaml",
    "41448.dict.yaml",
    "ext.dict.yaml",
    "tencent.dict.yaml",
];

/// 青简常用词表快照:补入白霜缺失的常用词,并给已收录的多字常用词提频。
/// 单字不参与提频,避免把「的/是/在」这类高频字继续放大成碎切激励。
const COMMON: &str = include_str!("../data/common.tsv");
/// 常用词先验倍数。青简常用词表是独立语料,给多字常用词一个温和的先验;
/// 先按 5 倍验证,句级/难例回归时再回调。
const COMMON_MULT: u64 = 5;
/// 口语异读:同一个词的另一读音,例如「这个」的 `zhei'ge`。白霜只收标准
/// 读音,`zheige` 会被拆成「贼+个」;这里只补 (词,音) 缺失的组合,词频沿用
/// 该词最高频读音,保证两种读法排序一致。
const READINGS: &str = include_str!("../data/readings.tsv");

pub(crate) fn run(args: Args) -> Result<(), String> {
    let mut best: HashMap<(String, String), u64> = HashMap::new();
    let mut unknown: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;
    let mut skipped = 0usize;
    for name in FILES {
        let path = args.frost.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("跳过缺失的 {name}");
            continue;
        };
        let (n, bad) = parse_frost(&text, &mut best, &mut unknown);
        total += n;
        skipped += bad;
        println!("{name}: {n} 行,坏 {bad} 行");
    }
    let (added, boosted, common_bad) = parse_common(COMMON, &mut best);
    println!("common.tsv: 补入 {added} 条,提频 {boosted} 条,坏 {common_bad} 行");
    if common_bad > 0 {
        return Err(format!(
            "common.tsv 有 {common_bad} 行无法解析;补词表坏了会让常用词静默消失"
        ));
    }
    if added + boosted == 0 {
        return Err("common.tsv 没有可用词条;检查 include_str 与文件格式".into());
    }
    let (readings, readings_bad) = parse_readings(READINGS, &mut best);
    println!("readings.tsv: 补入 {readings} 条异读,坏 {readings_bad} 行");
    if readings_bad > 0 {
        return Err(format!(
            "readings.tsv 有 {readings_bad} 行无法解析;异读表坏了会让口语输入静默退化"
        ));
    }
    if readings == 0 {
        return Err("readings.tsv 没有可用异读;检查 include_str 与文件格式".into());
    }
    // 音节表漏一个常用音,词条会被静默地当坏行丢掉(例如 `di` 曾漏掉 2.5 万词条,
    // 用户也打不出该音),所以这里把「未识别音节」显式暴露并在量级异常时中断。
    if !unknown.is_empty() {
        let mut top: Vec<(String, usize)> = unknown.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let list = top
            .iter()
            .take(10)
            .map(|(s, n)| format!("{s}×{n}"))
            .collect::<Vec<_>>()
            .join(",");
        println!("未识别音节: {list}");
        if let Some((s, n)) = top.first().filter(|(_, n)| *n >= 100) {
            return Err(format!(
                "音节表缺少常用音节 {s}({n} 次),这些词条会被静默丢弃;补 crates/dict/src/syllables.rs"
            ));
        }
    }
    let mut lines: Vec<(String, u64, String)> = best
        .into_iter()
        .map(|((word, key), freq)| (word, freq, key))
        .collect();
    lines.sort_by(|a, b| a.2.cmp(&b.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    let mut tsv = String::new();
    for (word, freq, key) in &lines {
        tsv.push_str(&format!("{word}\t{freq}\t{key}\n"));
    }
    std::fs::write(&args.out, &tsv).map_err(|e| format!("写 {}: {e}", args.out.display()))?;
    println!(
        "合计: 收录 {} 条(去重前 {total},坏行 {skipped}) → {}",
        lines.len(),
        args.out.display()
    );
    if let Some(bin) = &args.bin {
        // 与运行期同一条构造路径,保证二进制与 TSV 语义一致。
        let (dict, bad) = lueying_dict::Dictionary::from_text(&tsv);
        if bad != 0 {
            eprintln!("警告: 物化时跳过 {bad} 行");
        }
        let bytes = dict.to_binary();
        std::fs::write(bin, &bytes).map_err(|e| format!("写 {}: {e}", bin.display()))?;
        println!(
            "二进制: {} 条, {} MiB → {}",
            dict.len(),
            bytes.len() / (1024 * 1024),
            bin.display()
        );
    }
    Ok(())
}

/// 解析 Rime dict.yaml:跳过 `...` 之前的头,数据行为 `词<TAB>音节(空格分隔)<TAB>权重`。
fn parse_frost(
    text: &str,
    best: &mut HashMap<(String, String), u64>,
    unknown: &mut HashMap<String, usize>,
) -> (usize, usize) {
    let mut in_data = false;
    let (mut n, mut bad) = (0usize, 0usize);
    for line in text.lines() {
        if !in_data {
            if line.trim() == "..." {
                in_data = true;
            }
            continue;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(word), Some(code), weight) = (parts.next(), parts.next(), parts.next()) else {
            bad += 1;
            continue;
        };
        let sylls: Vec<&str> = code.split_whitespace().collect();
        let weight = weight
            .map(|w| w.trim().trim_end_matches('%'))
            .and_then(|w| w.parse::<u64>().ok())
            .unwrap_or(1);
        if word.chars().count() != sylls.len()
            || !sylls.iter().all(|s| is_syllable(&s.to_lowercase()))
        {
            for s in &sylls {
                let s = s.to_lowercase();
                if !is_syllable(&s) {
                    *unknown.entry(s).or_insert(0) += 1;
                }
            }
            bad += 1;
            continue;
        }
        let key = sylls
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join("'");
        let e = best.entry((word.to_string(), key)).or_insert(0);
        if weight > *e {
            *e = weight;
        }
        n += 1;
    }
    (n, bad)
}

/// `X的` ↔ `X得`:同音异形的孪生词。补词表给前者提频时,后者要同步提,
/// 否则「你说得有道理」会被「你说的有道理」顶掉。
fn de_twin(word: &str) -> Option<String> {
    if let Some(base) = word.strip_suffix('的') {
        Some(format!("{base}得"))
    } else {
        word.strip_suffix('得').map(|base| format!("{base}的"))
    }
}

/// 合并青简常用词表:多字词已收录则提频,未收录则补入;单字整行跳过。
/// 只认仓库内快照,CI 不依赖 work/ 下的参考项目。
fn parse_common(text: &str, best: &mut HashMap<(String, String), u64>) -> (usize, usize, usize) {
    let existing: HashSet<String> = best.keys().map(|(word, _)| word.clone()).collect();
    let mut entries: Vec<(String, String, u64)> = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(word), Some(freq), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
            bad += 1;
            continue;
        };
        let chars = word.chars().count();
        // 单字不提频,避免把「的/是/在」这类高频字继续放大成碎切激励。
        if chars < 2 || !word.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            continue;
        }
        let Ok(freq) = freq.parse::<u64>() else {
            bad += 1;
            continue;
        };
        let sylls: Vec<&str> = key.split('\'').collect();
        if sylls.len() != chars || !sylls.iter().all(|s| is_syllable(s)) {
            bad += 1;
            continue;
        }
        entries.push((word.to_string(), key.to_string(), freq));
    }
    let mut common_freq: HashMap<&str, u64> = HashMap::new();
    for (word, _, freq) in &entries {
        let slot = common_freq.entry(word.as_str()).or_insert(0);
        *slot = (*slot).max(*freq);
    }
    // 「X的」提频时,同音键的「X得」不提会把 `nishuodeyoudaoli` 推向
    // 「你说的有道理」。两者只靠语法模型分,词频比例保持白霜原样。
    let mut siblings: HashSet<String> = HashSet::new();
    for (word, key) in best.keys() {
        if !common_freq.contains_key(word.as_str()) {
            continue;
        }
        let Some(twin) = de_twin(word) else {
            continue;
        };
        if best.contains_key(&(twin.clone(), key.clone())) {
            siblings.insert(twin);
        }
    }
    let mut boosted = 0usize;
    for ((word, _), freq) in best.iter_mut() {
        if word.chars().count() < 2 {
            continue;
        }
        if let Some(&base) = common_freq.get(word.as_str()) {
            // 白霜里 0 频等同禁用;常用词表给了正频就把词启用,否则照常提频。
            *freq = if *freq == 0 {
                base
            } else {
                freq.saturating_mul(COMMON_MULT)
            };
            boosted += 1;
        } else if siblings.contains(word.as_str()) {
            *freq = freq.saturating_mul(COMMON_MULT);
            boosted += 1;
        }
    }
    drop(common_freq);
    drop(siblings);
    // 白霜已有这个词时只提频,不把青简的另一种读音追加进来;缺词才补一个最高频读音。
    let mut added = 0usize;
    for (word, key, freq) in entries {
        if !existing.contains(&word) {
            best.insert((word, key), freq);
            added += 1;
        }
    }
    (added, boosted, bad)
}

/// 合并口语异读表。格式 `词<TAB>音节`,两列;词频从该词已有的读音继承,
/// 因此异读与主读音同权。词或音节表里没有的词直接算坏行:异读表是手写的,
/// 静默跳过会让「zheige 打不出这个」这类退化难以发现。
fn parse_readings(text: &str, best: &mut HashMap<(String, String), u64>) -> (usize, usize) {
    let mut added = 0usize;
    let mut bad = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(word), Some(key)) = (parts.next(), parts.next()) else {
            bad += 1;
            continue;
        };
        let sylls: Vec<&str> = key.split('\'').collect();
        if !word.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            || sylls.len() != word.chars().count()
            || !sylls.iter().all(|s| is_syllable(s))
        {
            bad += 1;
            continue;
        }
        let ident = (word.to_string(), key.to_string());
        if best.contains_key(&ident) {
            continue;
        }
        let Some(freq) = best
            .iter()
            .filter(|((w, _), _)| w.as_str() == word)
            .map(|(_, freq)| *freq)
            .max()
        else {
            bad += 1;
            continue;
        };
        best.insert(ident, freq);
        added += 1;
    }
    (added, bad)
}
