//! 分段用户语料基准:照用户自己的空格切段,逐段打字、逐段上屏,看整句写对没有。
//!
//! 语料 `dicts/eval-user-v3-intervals.tsv` 每行是 `id<TAB>整句<TAB>分段拼音`,
//! 分段就是用户上屏的间隔。每段打完先跑与产品相同的重排决策,再按空格上屏,
//! 直到这一段落定。整句的上屏文字与标准句比较。

use std::fmt::Write as _;
use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_engine::{Event, Fuzzy, Key};
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::Models;
use crate::shared;

const CORPUS: &str = include_str!("../../../dicts/eval-user-v3-intervals.tsv");
/// 每段最多按几次空格落定,防止某段迟迟不收尾。
const MAX_COMMITS: usize = 8;
/// 报告里列出的错句数。
const MISSES_SHOWN: usize = 10;

/// 逐段打字并上屏,返回整句的上屏文字。每个句子用新引擎,不带学习。
fn write_sentence(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    rerankers: &mut (Rerank, Rerank),
    expected: &str,
    segments: &[&str],
) -> String {
    let mut engine = bench::make_engine(dict, gram, Fuzzy::auto());
    engine.set_learning(false);
    let (four, five) = rerankers;
    let mut written = String::new();
    for segment in segments {
        for c in segment.chars() {
            engine.process(Key::Letter(c));
        }
        for _ in 0..MAX_COMMITS {
            if engine.composition().is_none() {
                break;
            }
            shared::apply_rerank(&mut engine, four, five, expected);
            for event in engine.process(Key::Space) {
                if let Event::Commit(text) = event {
                    written.push_str(&text);
                }
            }
        }
    }
    written
}

pub fn run(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    models: &Models,
    report: &mut String,
) -> usize {
    let mut rerankers = (Rerank::new(&models.four), Rerank::new(&models.five));
    let mut sentences = 0usize;
    let mut exact = 0usize;
    let mut misses = Vec::new();
    for line in CORPUS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<&str> = line.split('\t').collect();
        let (Some(sentence), Some(intervals)) = (fields.get(1), fields.get(2)) else {
            continue;
        };
        let segments: Vec<&str> = intervals.split_whitespace().collect();
        let expected = bench::normalize(sentence);
        let written = write_sentence(dict, gram, &mut rerankers, &expected, &segments);
        sentences += 1;
        if bench::normalize(&written) == expected {
            exact += 1;
        } else {
            misses.push(format!("{expected} → {written}"));
        }
    }
    let percent = 100.0 * exact as f64 / sentences.max(1) as f64;
    let _ = writeln!(
        report,
        "\n## 分段用户语料(照空格上屏)\n\n整句完全正确 {exact} / {sentences}({percent:.1}%)\n"
    );
    for miss in misses.iter().take(MISSES_SHOWN) {
        let _ = writeln!(report, "- {miss}");
    }
    usize::from(sentences == 0)
}
