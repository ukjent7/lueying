//! 上屏后联想与按键节省模拟。
//!
//! 训练:把用户语料按词典反查切成词,喂给用户词库的转移记忆(不走上屏链路,
//! 这样训练成本与评测无关)。评测:同一切词口径下,逐对比较「继续打完整拼音」
//! 与「接受上屏后建议」。建议来自新增的 `Engine::predict_after`,它在上屏后
//! 立即按用户词序出 N 条,不要求组合里已经有前缀。
//!
//! 用法:
//! `lueying-bench predict --dict <主词典> [--train <语料>]... --test <语料> [--limit N]`

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use lueying_core::{Config, Engine, UserDict, set_sim_now};
use lueying_dict::Dictionary;

use crate::shared;

/// 切词时一个词最多几个字;更长短语留给整句，不作为下一词建议。
const MAX_WORD: usize = 4;
/// 训练遍数:两遍后转移层级达到联想门槛。
const TRAIN_PASSES: usize = 2;

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == flag)
        .filter_map(|(at, _)| args.get(at + 1).map(String::as_str))
        .collect()
}

fn key_len(key: &str) -> usize {
    key.chars().filter(|c| *c != '\'').count()
}

/// 词典反查:词 → 最高频读音。切词与建议读音都用这一份。
fn word_keys(dict: &Dictionary) -> HashMap<String, (String, u64)> {
    let mut out: HashMap<String, (String, u64)> = HashMap::new();
    for entry in dict.iter() {
        if entry.word.is_empty() || key_len(entry.key) == 0 {
            continue;
        }
        let slot = out
            .entry(entry.word.to_string())
            .or_insert_with(|| (entry.key.to_string(), entry.freq));
        if entry.freq > slot.1 {
            *slot = (entry.key.to_string(), entry.freq);
        }
    }
    out
}

/// 最长匹配切词;没有整词时按单字切,单字没有读音就不参与建议评测。
fn segment(text: &str, keys: &HashMap<String, (String, u64)>) -> Vec<(String, Option<String>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let mut found = None;
        for len in (1..=MAX_WORD.min(chars.len() - at)).rev() {
            let word: String = chars[at..at + len].iter().collect();
            if let Some((key, _)) = keys.get(&word) {
                found = Some((word, Some(key.clone())));
                break;
            }
        }
        let (word, key) = found.unwrap_or_else(|| (chars[at].to_string(), None));
        at += word.chars().count();
        out.push((word, key));
    }
    out
}

#[derive(Clone)]
struct Pair {
    previous: String,
    next: String,
    key: String,
}

/// 一条语料的「上一词 → 下一词」对。词级/分段语料自带上文：取上文最后一个词
/// 与期望词，这是真实的上屏后位置；没有上文的整句按相邻词切分。
fn pairs(case: &shared::Case<'_>, keys: &HashMap<String, (String, u64)>) -> Vec<Pair> {
    let expected = case.expected.trim();
    if !case.context.is_empty() {
        let Some((previous, _)) = segment(case.context, keys).pop() else {
            return Vec::new();
        };
        let Some((key, _)) = keys.get(expected) else {
            return Vec::new();
        };
        return vec![Pair {
            previous,
            next: expected.to_string(),
            key: key.clone(),
        }];
    }
    let words = segment(expected, keys);
    words
        .windows(2)
        .filter_map(|window| {
            let (previous, _) = &window[0];
            let (next, key) = &window[1];
            key.as_ref().map(|key| Pair {
                previous: previous.clone(),
                next: next.clone(),
                key: key.clone(),
            })
        })
        .collect()
}

fn read_cases(path: &str) -> Vec<shared::Case<'static>> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    shared::cases(leaked).0
}

pub fn run(args: &[String]) -> i32 {
    let Some(dict_path) = value(args, "--dict") else {
        eprintln!("predict 需要 --dict、--test,可重复 --train");
        return 2;
    };
    let Some(test_path) = value(args, "--test") else {
        eprintln!("predict 需要 --test 语料");
        return 2;
    };
    let limit = value(args, "--limit")
        .and_then(|text| text.parse().ok())
        .unwrap_or(usize::MAX);
    let Ok((dict, _)) = Dictionary::from_path(Path::new(dict_path)) else {
        eprintln!("词典加载失败 {dict_path}");
        return 2;
    };
    let keys = word_keys(&dict);
    let train: Vec<shared::Case<'static>> = values(args, "--train")
        .into_iter()
        .flat_map(read_cases)
        .collect();
    let test = read_cases(test_path);
    if train.is_empty() || test.is_empty() {
        eprintln!("训练或测试语料为空");
        return 2;
    }

    // 训练用户词序:每个词也学一次读音键，建议才能带着完整拼音键上屏。
    let mut user = UserDict::new();
    let mut clock = 1_800_000_000u64;
    for _ in 0..TRAIN_PASSES {
        for case in &train {
            set_sim_now(clock);
            clock += 120;
            for pair in pairs(case, &keys) {
                user.learn(&pair.key, &pair.next);
                user.learn_transitions(Some(&pair.previous), std::slice::from_ref(&pair.next));
            }
        }
    }
    let shared_user = Arc::new(RwLock::new(user));
    let mut engine = Engine::new(Arc::new(dict), Config::default()).with_user(shared_user);
    engine.set_learning(false);

    let mut pair_count = 0usize;
    let mut baseline_keys = 0usize;
    let mut saved_top1 = 0usize;
    let mut saved_top5 = 0usize;
    let mut hit1 = 0usize;
    let mut hit3 = 0usize;
    let mut hit5 = 0usize;
    let mut no_suggestion = 0usize;
    for case in test.iter().take(limit) {
        for pair in pairs(case, &keys) {
            let cost = key_len(&pair.key);
            if cost == 0 {
                continue;
            }
            pair_count += 1;
            baseline_keys += cost;
            let suggestions = engine.predict_after(&pair.previous, 5);
            let found = suggestions.iter().position(|word| word.text == pair.next);
            if suggestions.is_empty() {
                no_suggestion += 1;
            }
            match found {
                Some(0) => {
                    hit1 += 1;
                    hit3 += 1;
                    hit5 += 1;
                    saved_top1 += cost.saturating_sub(1);
                    saved_top5 += cost.saturating_sub(1);
                }
                Some(at) => {
                    if at < 3 {
                        hit3 += 1;
                    }
                    if at < 5 {
                        hit5 += 1;
                    }
                    // 建议栏内第 at+1 条仍是一键选中;只算一次按键。
                    saved_top5 += cost.saturating_sub(1);
                }
                None => {}
            }
        }
    }
    let pct = |part: usize, whole: usize| 100.0 * part as f64 / whole.max(1) as f64;
    let mut out = format!(
        "# 上屏后联想与按键节省\n\n训练 {} 句 / 测试 {} 句,{pair_count} 个相邻词对;切词最长 {MAX_WORD} 字。\n\n| 指标 | 数值 |\n|---|---:|\n| 建议命中 P@1 | {:.1}% |\n| 建议命中 P@3 | {:.1}% |\n| 建议命中 P@5 | {:.1}% |\n| 继续打完整拼音 | {} 键 |\n| 接受 P@1 建议 | {} 键 |\n| 接受 P@5 建议 | {} 键 |\n| P@1 按键节省 | {:.1}% |\n| P@5 按键节省 | {:.1}% |\n| 无建议词对 | {} 对 |\n",
        train.len(),
        test.len().min(limit),
        pct(hit1, pair_count),
        pct(hit3, pair_count),
        pct(hit5, pair_count),
        baseline_keys,
        baseline_keys - saved_top1,
        baseline_keys - saved_top5,
        pct(saved_top1, baseline_keys),
        pct(saved_top5, baseline_keys),
        no_suggestion,
    );
    out.push_str("\n> 建议栏内命中按一次按键上屏计;未命中时按继续打完整拼音计。训练只构造用户词序与读音键，不读上屏链路。\n");
    let _ = std::fs::create_dir_all("target");
    let _ = std::fs::write("target/predict.md", &out);
    print!("{out}\n报告: target/predict.md\n");
    0
}
