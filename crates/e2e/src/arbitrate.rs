//! 读音仲裁:金标读音语料上模拟真实打字,逐词核对现役词典的读音。
//!
//! 「哪个读音是对的」在输入法里只有一个可检验的答案:用户打出那个拼音,
//! 现役引擎(解码 + 8M 重排)把这个词送回首选。这里拿与构建无关的金标准
//! 语料(sentences-ctx-v1,每句带逐字读音),按词典最长匹配切词,对每个
//! 多字词出现处,把该词在词典里的全部读音与金标读音各模拟打一遍(上下文
//! = 金标句子的真实前缀),统计哪个读音能把它送回首选。
//!
//! 只记录分歧,不改任何东西:产出逐词裁决表与一致率,供换注音的影子对照。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::sync::Arc;

use flate2::read::GzDecoder;
use serde_json::json;

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_dict::syllables::is_syllable;
use lueying_engine::{Engine, Fuzzy, Key};
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::Models;

/// 与词典门禁(evaluate.py)同一把尺:切词窗口 8 字。
const SEGMENT_WINDOW: usize = 8;
/// 一个读音至少出现这么多次才给裁决;更少记「证据不足」,不进一致率。
const MIN_OCCURRENCES: u32 = 3;
/// 片内用例数:共用一个引擎(学习关掉),片间状态不互通。
const CHUNK: usize = 256;

/// 一个读音在一批上下文里的表现。
#[derive(Default, Clone, Copy)]
struct KeyStat {
    occurrences: u32,
    top1: u32,
}

impl KeyStat {
    fn rate(self) -> f64 {
        self.top1 as f64 / self.occurrences.max(1) as f64
    }
}

/// 与 zlib.crc32 一致;只用来复刻语料的奇偶对半切分。
fn crc32(text: &str) -> u32 {
    let mut crc = !0u32;
    for byte in text.as_bytes() {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB88320 & mask);
        }
    }
    !crc
}

/// 一次「真实打字」:设上文、敲按键、走与产品同一门控的 8M 重排,回首选。
fn typed_top1(engine: &mut Engine, four: &mut Rerank, prefix: &str, typed: &str) -> Option<String> {
    engine.process(Key::Esc);
    if prefix.is_empty() {
        engine.clear_context();
    } else {
        engine.set_context(prefix);
    }
    for c in typed.chars() {
        engine.process(Key::Letter(c));
    }
    // 快捷/固定项(标点、网址)不参与重排,与产品同一口径。
    if engine
        .candidates_detailed()
        .iter()
        .any(|cand| cand.quick || cand.pinned)
    {
        return bench::eval_texts(engine).first().map(|s| (*s).to_string());
    }
    let snap = crate::rerank::snapshot(engine);
    if !snap.context.is_empty() {
        let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
        if let Some(index) = four.decide(&snap.context, &texts, &snap.facts) {
            engine.promote(index);
        }
    }
    bench::eval_texts(engine).first().map(|s| (*s).to_string())
}

/// 逐条金标用例:`gold` 全汉字、`readings` 逐字对齐、unit 落在奇数半
/// (偶数半是构建的开发集,门禁 evaluate.py 同样只用奇数半)。
struct Case {
    gold: Vec<char>,
    readings: Vec<String>,
}

fn read_cases(path: &str) -> Vec<Case> {
    let file = fs::File::open(path).unwrap_or_else(|error| panic!("读语料 {path}: {error}"));
    let mut raw = Vec::new();
    GzDecoder::new(file)
        .read_to_end(&mut raw)
        .unwrap_or_else(|error| panic!("解压语料 {path}: {error}"));
    let text = String::from_utf8(raw).unwrap_or_else(|error| panic!("语料不是 UTF-8: {error}"));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let case: serde_json::Value = serde_json::from_str(line).ok()?;
            if case["style"].as_str() != Some("full") {
                return None;
            }
            let unit = case["unit"].as_str()?;
            if crc32(unit) % 2 != 1 {
                return None;
            }
            let gold_text = case["gold"].as_str()?;
            let gold: Vec<char> = gold_text.chars().collect();
            if gold.is_empty() || !gold.iter().all(|c| ('\u{4e00}'..='\u{9fff}').contains(c)) {
                return None;
            }
            let readings: Vec<String> = case["readings"]
                .as_array()?
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect();
            if readings.len() != gold.len() {
                return None;
            }
            Some(Case { gold, readings })
        })
        .collect()
}

/// 词典的两种视图:最长匹配的词形集合,与多字词的全部读音(主读音按频次排前)。
struct DictIndex {
    forms: HashSet<String>,
    words: HashMap<String, Vec<(String, u64)>>,
}

impl DictIndex {
    fn open(dict: &Dictionary) -> Self {
        let mut forms = HashSet::new();
        let mut words: HashMap<String, Vec<(String, u64)>> = HashMap::new();
        for entry in dict.iter() {
            forms.insert(entry.word.to_string());
            if entry.word.chars().count() >= 2 {
                words
                    .entry(entry.word.to_string())
                    .or_default()
                    .push((entry.key.to_string(), entry.freq));
            }
        }
        for keys in words.values_mut() {
            keys.sort_by_key(|(_, freq)| std::cmp::Reverse(*freq));
        }
        Self { forms, words }
    }

    /// 词典里频次最高的读音;没有这个词返回 None。
    fn primary(&self, word: &str) -> Option<&str> {
        self.words
            .get(word)
            .and_then(|keys| keys.first())
            .map(|(key, _)| key.as_str())
    }
}

/// 一片用例:切词、模拟打字、聚合每个读音的表现。
fn run_chunk(
    cases: &[Case],
    index: &DictIndex,
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    models: &Models,
    stats: &mut HashMap<String, HashMap<String, KeyStat>>,
    samples: &mut HashMap<String, String>,
) {
    let mut engine = bench::make_engine(dict, gram, Fuzzy::auto());
    engine.set_learning(false);
    let mut four = Rerank::new(&models.four);
    for case in cases {
        let n = case.gold.len();
        let text: String = case.gold.iter().collect();
        let mut i = 0;
        while i < n {
            let window = SEGMENT_WINDOW.min(n - i);
            let matched = (1..=window).rev().find_map(|len| {
                let piece: String = case.gold[i..i + len].iter().collect();
                index.forms.contains(&piece).then_some((piece, i + len))
            });
            let Some((word, end)) = matched else {
                i += 1;
                continue;
            };
            let width = word.chars().count();
            let start = i;
            i = end;
            if width == 1 {
                continue;
            }
            let Some(keys) = index.words.get(&word) else {
                continue;
            };
            let start_byte = text
                .char_indices()
                .nth(start)
                .map_or(text.len(), |(at, _)| at);
            let prefix = &text[..start_byte];
            let gold_key = case.readings[start..start + width].join("'");
            let mut candidates: Vec<&str> = keys.iter().map(|(key, _)| key.as_str()).collect();
            if !candidates.contains(&gold_key.as_str()) {
                candidates.push(&gold_key);
            }
            for key in candidates {
                // 打给引擎的是连续小写字母;音节不合法的读音没法真实输入,跳过。
                let syls: Vec<&str> = key.split('\'').collect();
                if syls.len() != width
                    || !key.chars().all(|c| c.is_ascii_lowercase() || c == '\'')
                    || !syls.iter().all(|syl| is_syllable(syl))
                {
                    continue;
                }
                let typed = key.replace('\'', "");
                let top = typed_top1(&mut engine, &mut four, prefix, &typed);
                let entry = stats
                    .entry(word.clone())
                    .or_default()
                    .entry(key.to_string())
                    .or_default();
                entry.occurrences += 1;
                entry.top1 += u32::from(top.as_deref() == Some(word.as_str()));
                if top.as_deref() != Some(word.as_str()) && !samples.contains_key(&word) {
                    samples.insert(
                        word.clone(),
                        format!(
                            "{prefix}□{word} 打 {typed} 得 {:?}",
                            top.unwrap_or_default()
                        ),
                    );
                }
            }
        }
    }
}

/// 跑全部语料,写 JSON 裁决表并回一段人话摘要。
pub(crate) fn run(
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: &Models,
    corpus_path: &str,
    out_path: &str,
) -> String {
    let index = DictIndex::open(&dict);
    let cases = read_cases(corpus_path);
    let mut stats: HashMap<String, HashMap<String, KeyStat>> = HashMap::new();
    let mut samples: HashMap<String, String> = HashMap::new();
    for chunk in cases.chunks(CHUNK) {
        run_chunk(
            chunk,
            &index,
            &dict,
            &gram,
            models,
            &mut stats,
            &mut samples,
        );
    }

    // 裁决:出现足够的读音里,首选率最高的那个;并列时词典主读音优先。
    let mut words = Vec::new();
    for (word, keys) in &stats {
        let Some(primary) = index.primary(word) else {
            continue;
        };
        let mut judged: Vec<(&String, &KeyStat)> = keys
            .iter()
            .filter(|(_, stat)| stat.occurrences >= MIN_OCCURRENCES)
            .collect();
        judged.sort_by(|a, b| {
            b.1.rate()
                .total_cmp(&a.1.rate())
                // 并列时词典主读音排前:不改现状优先。
                .then_with(|| (b.0.as_str() == primary).cmp(&(a.0.as_str() == primary)))
                .then_with(|| a.0.cmp(b.0))
        });
        let Some((arbiter, _)) = judged.first() else {
            continue;
        };
        words.push(json!({
            "word": word,
            "dict_primary": primary,
            "arbiter": arbiter,
            "agree": arbiter.as_str() == primary,
            "keys": keys.iter().map(|(key, stat)| json!({
                "key": key,
                "n": stat.occurrences,
                "top1": stat.top1,
                "rate": (stat.rate() * 1000.0).round() / 10.0,
            })).collect::<Vec<_>>(),
            "sample": samples.get(word),
        }));
    }
    words.sort_by(|a, b| a["word"].as_str().cmp(&b["word"].as_str()));
    let verdicts = words.len();
    let agreed = words.iter().filter(|w| w["agree"] == json!(true)).count();
    let agree_rate = if verdicts == 0 {
        0.0
    } else {
        agreed as f64 * 100.0 / verdicts as f64
    };
    let disagreements = words
        .iter()
        .filter(|w| w["agree"] == json!(false))
        .take(80)
        .cloned()
        .collect::<Vec<_>>();
    let report = json!({
        "meta": {
            "corpus": corpus_path,
            "cases": cases.len(),
            "min_occurrences": MIN_OCCURRENCES,
        },
        "summary": {
            "words": words.len(),
            "verdicts": verdicts,
            "agree": agreed,
            "agree_rate": (agree_rate * 10.0).round() / 10.0,
        },
        "disagreements": disagreements,
        "words": words,
    });
    let mut summary = format!(
        "\n## 读音仲裁(影子,只记录不改动)\n\n金标读音语料 {} 句;多字词 {} 个有裁决,与现役词典一致 {agreed}({agree_rate:.1}%)。\n\n分歧样本(词 / 词典主读音 → 仲裁):\n",
        cases.len(),
        verdicts,
    );
    for item in &disagreements {
        summary.push_str(&format!(
            "- {} {} → {}\n",
            item["word"].as_str().unwrap_or_default(),
            item["dict_primary"].as_str().unwrap_or_default(),
            item["arbiter"].as_str().unwrap_or_default(),
        ));
    }
    let _ = fs::write(
        out_path,
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    );
    summary
}
