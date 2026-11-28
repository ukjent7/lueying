//! 路线 3 噪声套件:从干净全拼音节派生模糊、邻键、转置与中英混合四组输入。
//!
//! 派生只依赖固定哈希,同一份语料与同一组参数永远得到同一批输入,因此改前/改后
//! 两个二进制可以对照同一份样本。单段输入沿用 `conversion` 的排名口径;中英混合
//! 按真实上屏顺序模拟:先打中文段并选中期望候选,再打英文/数字段,要求两段都上屏
//! 成期望文字。
//!
//! 用法:
//! `lueying-bench noise --dict <主词典> [--gram <搭配表>] [--rerank-dir <模型目录>]
//!  --corpus <干净全拼语料.tsv> [--per-group <每组条数>]`

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lueying_core::{Engine, Event, Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::{self, Models};
use crate::shared;

const DEFAULT_PER_GROUP: usize = 200;
const KEY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const SHIFT_BY_ROW: [f32; 3] = [0.0, 0.5, 1.0];
const FUZZY_INITIAL: &[(&str, &str)] = &[
    ("zh", "z"),
    ("ch", "c"),
    ("sh", "s"),
    ("l", "n"),
    ("f", "h"),
];
const FUZZY_FINAL: &[(&str, &str)] = &[
    ("an", "ang"),
    ("en", "eng"),
    ("in", "ing"),
    ("ian", "iang"),
    ("uan", "uang"),
];
/// 中英混合里使用的英文词及词表给出的规范写法。都是常见输入,不依赖用户词库。
const ENGLISH: &[(&str, &str)] = &[
    ("hello", "hello"),
    ("world", "world"),
    ("music", "music"),
    ("team", "team"),
    ("test", "test"),
    ("code", "code"),
    ("phone", "phone"),
    ("email", "email"),
    ("online", "online"),
    ("update", "update"),
    ("ok", "OK"),
    ("app", "App"),
    ("wifi", "Wi-Fi"),
    ("iphone", "iPhone"),
    ("vscode", "VSCode"),
];
const DIGITS: &str = "2026";

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn hash(parts: &[&str]) -> u64 {
    let mut value = 0xcbf2_9ce4_8422_2325u64;
    for part in parts {
        for byte in part.bytes() {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x100_0000_01b3);
        }
        value ^= 0xff;
        value = value.wrapping_mul(0x100_0000_01b3);
    }
    value
}

fn pick<T>(items: &[T], seed: u64) -> Option<&T> {
    (!items.is_empty()).then(|| &items[seed as usize % items.len()])
}

fn neighbors(c: char) -> Vec<char> {
    let Some((row, col)) = KEY_ROWS
        .iter()
        .enumerate()
        .find_map(|(row, keys)| keys.chars().position(|key| key == c).map(|col| (row, col)))
    else {
        return Vec::new();
    };
    let x = col as f32 + SHIFT_BY_ROW[row];
    let mut out = Vec::new();
    for (other_row, keys) in KEY_ROWS.iter().enumerate() {
        if other_row.abs_diff(row) > 1 {
            continue;
        }
        for (other_col, key) in keys.chars().enumerate() {
            if key == c {
                continue;
            }
            let dx = x - (other_col as f32 + SHIFT_BY_ROW[other_row]);
            let dy = row as f32 - other_row as f32;
            if (dx * dx + dy * dy).sqrt() <= 1.15 {
                out.push(key);
            }
        }
    }
    out
}

fn clean_source(case: &shared::Case<'_>) -> bool {
    let unit = case.category.split('-').next();
    let reading = case.category.split('-').nth(1);
    matches!(unit, Some("word" | "chunk" | "clause"))
        && matches!(reading, Some("full"))
        && !case.input.is_empty()
        && case
            .input
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'\'')
        && !case.expected.is_empty()
        && case
            .expected
            .chars()
            .all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

fn fuzzy(input: &str, seed: u64) -> Option<String> {
    let mut choices: Vec<(usize, usize, &str)> = Vec::new();
    for (source, target) in FUZZY_INITIAL.iter().chain(FUZZY_FINAL) {
        for (at, _) in input.match_indices(source) {
            choices.push((at, at + source.len(), target));
        }
    }
    let (start, end, target) = *pick(&choices, seed)?;
    let mut out = input.to_string();
    out.replace_range(start..end, target);
    (out != input).then_some(out)
}

fn neighbor(input: &str, seed: u64) -> Option<String> {
    let positions: Vec<usize> = input
        .bytes()
        .enumerate()
        .filter(|(_, b)| b.is_ascii_lowercase())
        .map(|(at, _)| at)
        .collect();
    let at = *pick(&positions, seed)?;
    let c = input.as_bytes()[at] as char;
    let options = neighbors(c);
    let key = *pick(&options, seed >> 8)?;
    let mut out = input.to_string();
    out.replace_range(at..at + 1, &key.to_string());
    (out != input).then_some(out)
}

fn transpose(input: &str, seed: u64) -> Option<String> {
    let bytes = input.as_bytes();
    let positions: Vec<usize> = (0..bytes.len().saturating_sub(1))
        .filter(|&at| {
            bytes[at].is_ascii_lowercase()
                && bytes[at + 1].is_ascii_lowercase()
                && bytes[at] != bytes[at + 1]
        })
        .collect();
    let at = *pick(&positions, seed)?;
    let mut out = input.to_string();
    out.replace_range(
        at..at + 2,
        &format!("{}{}", bytes[at + 1] as char, bytes[at] as char),
    );
    Some(out)
}

#[derive(Clone)]
struct Mixed {
    /// 中文段输入与期望。
    cn_input: String,
    cn_expected: String,
    /// 英文段输入与词表规范写法。
    en_input: String,
    en_expected: String,
}

#[derive(Clone)]
struct Case {
    category: &'static str,
    input: String,
    expected: String,
    context: String,
    mixed: Option<Mixed>,
}

#[derive(Default)]
struct Bucket {
    total: usize,
    top1: usize,
    top5: usize,
    /// 混合套件:中文段、英文段与整段自动上屏分别命中多少。
    cn1: usize,
    en1: usize,
    whole: usize,
}

impl Bucket {
    fn add(&mut self, rank: i64) {
        self.total += 1;
        self.top1 += usize::from(rank == 0);
        self.top5 += usize::from((0..5).contains(&rank));
    }
}

fn derive(cases: &[shared::Case<'_>], per_group: usize) -> Vec<Case> {
    let mut source: Vec<&shared::Case<'_>> =
        cases.iter().filter(|case| clean_source(case)).collect();
    source.sort_by_key(|case| hash(&[case.category, case.input, case.expected]));
    let mut out: Vec<Case> = Vec::new();
    let mut counts = [0usize; 4];
    for case in source {
        let seed = hash(&["route3", case.input, case.expected]);
        let salt = |name: &str| hash(&[name, case.input, case.expected]);
        for (index, (name, derived)) in [
            ("模糊", fuzzy(case.input, salt("fuzzy"))),
            ("邻键", neighbor(case.input, salt("neighbor"))),
            ("转置", transpose(case.input, salt("transpose"))),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(input) = derived else {
                continue;
            };
            if counts[index] >= per_group {
                continue;
            }
            counts[index] += 1;
            out.push(Case {
                category: match name {
                    "模糊" => "模糊",
                    "邻键" => "邻键",
                    _ => "转置",
                },
                input,
                expected: case.expected.to_string(),
                context: case.context.to_string(),
                mixed: None,
            });
        }
        if counts[3] < per_group {
            let (en_input, en_expected) = *pick(ENGLISH, seed).expect("英文词表非空");
            counts[3] += 1;
            out.push(Case {
                category: "中英混合",
                input: format!("{}|{en_input}|{DIGITS}", case.input),
                expected: format!("{}|{en_expected}|{DIGITS}", case.expected),
                context: case.context.to_string(),
                mixed: Some(Mixed {
                    cn_input: case.input.to_string(),
                    cn_expected: case.expected.to_string(),
                    en_input: en_input.to_string(),
                    en_expected: en_expected.to_string(),
                }),
            });
        }
        if counts.iter().all(|&count| count >= per_group) {
            break;
        }
    }
    out
}

fn rank(engine: &Engine, expected: &str) -> i64 {
    shared::rank_of(engine, &bench::normalize(expected))
}

fn commit(engine: &mut Engine) -> String {
    engine
        .process(Key::Space)
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect()
}

fn mixed_rank(engine: &mut Engine, mixed: &Mixed) -> (i64, i64, bool) {
    // 先打中文段,选中期望候选以隔离英文/数字段;期望中文不在候选里时按首选上屏,
    // 整段判定自然失败,不把中文错误算到英文直通头上。
    for c in mixed.cn_input.chars() {
        engine.process(Key::Letter(c));
    }
    let cn_rank = rank(engine, &mixed.cn_expected);
    let index = engine.candidates_detailed().iter().position(|cand| {
        bench::matches_normalized(&cand.text, &bench::normalize(&mixed.cn_expected))
    });
    let picked = match index {
        Some(index) => engine
            .pick(index)
            .into_iter()
            .filter_map(|event| match event {
                Event::Commit(text) => Some(text),
                _ => None,
            })
            .collect::<String>(),
        None => commit(engine),
    };
    let cn_ok = picked == mixed.cn_expected;

    for c in mixed.en_input.chars() {
        engine.process(Key::Letter(c));
    }
    let en_rank = rank(engine, &mixed.en_expected);
    let en_top = commit(engine);
    let en_ok = en_top == mixed.en_expected;

    let mut digits = String::new();
    for c in DIGITS.chars() {
        let digit = c.to_digit(10).unwrap_or(0) as u8;
        for event in engine.process(Key::Digit(digit)) {
            if let Event::Passthrough(Key::Digit(d)) = event {
                digits.push(char::from_digit(u32::from(d), 10).unwrap_or('0'));
            }
        }
    }
    (cn_rank, en_rank, cn_ok && en_ok && digits == DIGITS)
}

pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(corpus_path)) = (value(args, "--dict"), value(args, "--corpus"))
    else {
        eprintln!("noise 需要 --dict 与 --corpus(可选 --gram、--rerank-dir、--per-group)");
        return 2;
    };
    let per_group = value(args, "--per-group")
        .and_then(|text| text.parse().ok())
        .unwrap_or(DEFAULT_PER_GROUP);
    let Ok((dict, _)) = Dictionary::from_path(Path::new(dict_path)) else {
        eprintln!("词典加载失败 {dict_path}");
        return 2;
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let models: Option<Models> =
        value(args, "--rerank-dir").and_then(|dir| match rerank::load(Path::new(dir)) {
            Ok(models) => Some(models),
            Err(error) => {
                eprintln!("模型加载失败 {dir}: {error}");
                None
            }
        });
    let Ok(body) = std::fs::read_to_string(corpus_path) else {
        eprintln!("读语料失败 {corpus_path}");
        return 2;
    };
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let (cases, malformed) = shared::cases(leaked);
    if cases.is_empty() {
        eprintln!("语料没有可用条目 {corpus_path}");
        return 2;
    }
    let derived = derive(&cases, per_group);
    let dict = Arc::new(dict);
    let mut four = models.as_ref().map(|models| Rerank::new(&models.four));
    let mut five = models.as_ref().map(|models| Rerank::new(&models.five));
    let mut buckets: Vec<(&'static str, Bucket)> = Vec::new();
    let mut typing_ms = Vec::new();
    let mut eight_ms = Vec::new();
    let mut five_ms = Vec::new();
    let mut rows = String::from(
        "category\tinput\texpected\tcontext_chars\trank_engine\tcn_rank\ten_rank\tms_typing\n",
    );
    for case in &derived {
        let mut engine: Engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
        engine.set_learning(false);
        engine.process(Key::Esc);
        if case.context.is_empty() {
            engine.clear_context();
        } else {
            engine.set_context(&case.context);
        }
        let (rank_engine, cn_rank, en_rank, ms) = if let Some(mixed) = &case.mixed {
            let started = Instant::now();
            let (cn_rank, en_rank, whole) = mixed_rank(&mut engine, mixed);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            typing_ms.push(ms);
            (if whole { 0 } else { -1 }, cn_rank, en_rank, ms)
        } else {
            for c in case.input.chars() {
                let started = Instant::now();
                engine.process(Key::Letter(c));
                typing_ms.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            let started = Instant::now();
            let mut engine_rank = rank(&engine, &case.expected);
            if let (Some(four), Some(five)) = (four.as_mut(), five.as_mut()) {
                let snap = rerank::snapshot(&engine);
                let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
                let at = Instant::now();
                let promote = (!snap.context.is_empty())
                    .then(|| four.decide(&snap.context, &texts, &snap.facts))
                    .flatten();
                eight_ms.push(at.elapsed().as_secs_f64() * 1000.0);
                if let Some(index) = promote {
                    engine.promote(index);
                }
                let snap = rerank::snapshot(&engine);
                let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
                let at = Instant::now();
                let promote = five.decide_settled_prefiltered(
                    &snap.context,
                    &texts,
                    &snap.facts,
                    None,
                    Some(&mut *four),
                );
                five_ms.push(at.elapsed().as_secs_f64() * 1000.0);
                if let Some(index) = promote {
                    engine.promote(index);
                }
                engine_rank = rank(&engine, &case.expected);
            }
            (
                engine_rank,
                -1,
                -1,
                started.elapsed().as_secs_f64() * 1000.0,
            )
        };
        let bucket = match buckets.iter_mut().find(|(name, _)| *name == case.category) {
            Some((_, bucket)) => bucket,
            None => {
                buckets.push((case.category, Bucket::default()));
                &mut buckets.last_mut().expect("刚插入").1
            }
        };
        if case.mixed.is_some() {
            bucket.total += 1;
            bucket.cn1 += usize::from(cn_rank == 0);
            bucket.en1 += usize::from(en_rank == 0);
            bucket.whole += usize::from(rank_engine == 0);
        } else {
            bucket.add(rank_engine);
        }
        let _ = writeln!(
            rows,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}",
            case.category,
            case.input.replace(['\t', '\n'], " "),
            case.expected.replace(['\t', '\n'], " "),
            case.context.chars().count(),
            rank_engine,
            cn_rank,
            en_rank,
            ms
        );
    }
    buckets.sort_by(|a, b| a.0.cmp(b.0));
    let name = Path::new(corpus_path)
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let mut out = format!(
        "# 路线 3 噪声套件({name})\n\n语料 {corpus_path},每组 {per_group} 条,坏行 {malformed};学习关闭。\n\n| 分组 | 样本 | P@1 | P@5 | 中文段 P@1 | 英文段 P@1 | 整段正确 |\n|---|---:|---:|---:|---:|---:|---:|\n"
    );
    for (category, bucket) in &buckets {
        let rate = |part: usize| {
            if bucket.total == 0 {
                "-".to_string()
            } else {
                format!("{:.1}%", 100.0 * part as f64 / bucket.total as f64)
            }
        };
        let _ = writeln!(
            out,
            "| {category} | {} | {} | {} | {} | {} | {} |",
            bucket.total,
            if *category == "中英混合" {
                "-".to_string()
            } else {
                rate(bucket.top1)
            },
            if *category == "中英混合" {
                "-".to_string()
            } else {
                rate(bucket.top5)
            },
            if *category == "中英混合" {
                rate(bucket.cn1)
            } else {
                "-".to_string()
            },
            if *category == "中英混合" {
                rate(bucket.en1)
            } else {
                "-".to_string()
            },
            if *category == "中英混合" {
                rate(bucket.whole)
            } else {
                "-".to_string()
            },
        );
    }
    let _ = writeln!(
        out,
        "\n## 延迟(逐键,不含重排)\n\n| 指标 | 毫秒 |\n|---|---:|\n| 按键数 | {} |\n| p50 | {:.2} |\n| p95 | {:.2} |\n| p99 | {:.2} |",
        typing_ms.len(),
        shared::pct(&mut typing_ms, 50.0),
        shared::pct(&mut typing_ms, 95.0),
        shared::pct(&mut typing_ms, 99.0),
    );
    if models.is_some() {
        let _ = writeln!(
            out,
            "\n| 重排 | p50 | p95 | p99 |\n|---|---:|---:|---:|\n| 8M 同步 | {:.2} | {:.2} | {:.2} |\n| 25M 停顿 | {:.2} | {:.2} | {:.2} |",
            shared::pct(&mut eight_ms, 50.0),
            shared::pct(&mut eight_ms, 95.0),
            shared::pct(&mut eight_ms, 99.0),
            shared::pct(&mut five_ms, 50.0),
            shared::pct(&mut five_ms, 95.0),
            shared::pct(&mut five_ms, 99.0),
        );
    }
    let _ = std::fs::create_dir_all("target/bench");
    let raw_path = format!("target/bench/noise-{name}.tsv");
    if let Err(error) = std::fs::write(&raw_path, rows) {
        eprintln!("写 {raw_path} 失败: {error}");
        return 1;
    }
    let md_path = format!("target/noise-{name}.md");
    let _ = std::fs::write(&md_path, &out);
    print!("{out}\n逐条结果: {raw_path}\n报告: {md_path}\n");
    0
}

#[cfg(test)]
mod tests {
    use super::{fuzzy, hash, neighbor, transpose};

    #[test]
    fn fuzzy_respects_directional_rules() {
        // For nihao, n and h are targets, not sources.
        assert_eq!(fuzzy("nihao", hash(&["nihao", "f"])), None);
        for input in ["jintiantianqi", "shurufa"] {
            assert!(
                fuzzy(input, hash(&[input, "f"])).is_some_and(|text| text != input),
                "fuzzy: {input}"
            );
        }
    }

    #[test]
    fn neighbor_and_transpose_change_the_input() {
        for input in ["nihao", "jintiantianqi", "shurufa"] {
            let changed = |out: Option<String>| out.is_some_and(|text| text != input);
            assert!(changed(neighbor(input, hash(&[input, "n"]))));
            assert!(changed(transpose(input, hash(&[input, "t"]))));
        }
    }
}
