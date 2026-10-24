//! 词典数据管线:`dict` 子命令把白霜拼音(Rime dict.yaml)转成本项目的 TSV 词典。
//!
//! 用法:`cargo run -p xtask -- dict --frost <白霜cn_dicts目录> --out <输出.tsv>`

use std::collections::HashMap;
use std::path::PathBuf;

use pinyin_dict::syllables::is_syllable;

struct Args {
    frost: PathBuf,
    out: PathBuf,
}

fn main() {
    let mut args = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "dict" => args = Some(parse_dict(&mut it)),
            other => {
                eprintln!("未知子命令或参数: {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(args) = args else {
        eprintln!("用法: xtask dict --frost <dir> --out <file>");
        std::process::exit(2);
    };
    if let Err(e) = run(args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn parse_dict(it: &mut impl Iterator<Item = String>) -> Args {
    let mut frost = None;
    let mut out = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frost" => frost = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    Args {
        frost: frost.expect("--frost 需要 <白霜 cn_dicts 目录>"),
        out: out.expect("--out 需要 <输出文件>"),
    }
}

/// 白霜的词表文件,按质量从高到低。
const FILES: &[&str] = &[
    "base.dict.yaml",
    "8105.dict.yaml",
    "41448.dict.yaml",
    "ext.dict.yaml",
    "tencent.dict.yaml",
];

fn run(args: Args) -> Result<(), String> {
    let mut best: HashMap<(String, String), u64> = HashMap::new();
    let mut total = 0usize;
    let mut skipped = 0usize;
    for name in FILES {
        let path = args.frost.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("跳过缺失的 {name}");
            continue;
        };
        let (n, bad) = parse_frost(&text, &mut best);
        total += n;
        skipped += bad;
        println!("{name}: {n} 行,坏 {bad} 行");
    }
    let mut lines: Vec<(String, u64, String)> = best
        .into_iter()
        .map(|((word, key), freq)| (word, freq, key))
        .collect();
    lines.sort_by(|a, b| a.2.cmp(&b.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    let mut out = String::new();
    for (word, freq, key) in &lines {
        out.push_str(&format!("{word}\t{freq}\t{key}\n"));
    }
    std::fs::write(&args.out, out).map_err(|e| format!("写 {}: {e}", args.out.display()))?;
    println!(
        "合计: 收录 {} 条(去重前 {total},坏行 {skipped}) → {}",
        lines.len(),
        args.out.display()
    );
    Ok(())
}

/// 解析 Rime dict.yaml:跳过 `...` 之前的头,数据行为 `词<TAB>音节(空格分隔)<TAB>权重`。
fn parse_frost(text: &str, best: &mut HashMap<(String, String), u64>) -> (usize, usize) {
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
            bad += 1;
            continue;
        }
        let key = sylls
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join("'");
        let e = best
            .entry((word.to_string(), key))
            .or_insert(0);
        if weight > *e {
            *e = weight;
        }
        n += 1;
    }
    (n, bad)
}
