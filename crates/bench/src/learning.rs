//! 学习基准:走产品的重排链路,模拟用户反复选同一个词。
//!
//! 目标是有上文、答案在候选里但不在首选的用例。每个目标用全新的引擎,连选
//! [`ROUNDS`] 次,每次选完量一次名次。同键对照是开发集里输入相同、答案不同且
//! 原本正确的用例:学完目标后再量,首选变了就算误伤。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use lueying_core::{Engine, Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::Models;
use crate::shared::{self, Case};

const CORPUS: &str = include_str!("../../../dicts/eval-dev-v4.tsv");
/// 目标数与最多扫描的用例数。
const TARGETS: usize = 120;
const SCAN: usize = 800;
const ROUNDS: usize = 3;
/// 报告里列出的学不会样本数。
const STUCK_SHOWN: usize = 10;

struct Rerankers {
    four: Rerank,
    five: Rerank,
}

/// 在用例的上文里打完输入并跑完两档重排,返回答案名次(-1 表示不在候选里)。
fn rank(engine: &mut Engine, models: &mut Rerankers, case: &Case, expected: &str) -> i64 {
    engine.process(Key::Esc);
    engine.set_context(case.context);
    for c in case.input.chars() {
        engine.process(Key::Letter(c));
    }
    shared::apply_rerank(engine, &mut models.four, &mut models.five, expected);
    shared::rank_of(engine, expected)
}

/// 选中当前组合里的答案;候选里没有就返回 false。
fn choose(engine: &mut Engine, expected: &str) -> bool {
    let word = bench::eval_texts(engine)
        .into_iter()
        .find(|text| bench::matches_normalized(text, expected))
        .map(str::to_string);
    word.is_some_and(|word| bench::pick(engine, &word))
}

fn rate(part: usize, total: usize) -> f64 {
    100.0 * part as f64 / total.max(1) as f64
}

pub fn run(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    models: &Models,
    report: &mut String,
) -> usize {
    let (all, _) = shared::cases(CORPUS);
    // 只取有上文的短语与词级用例;整句的上文很长,学习信号在别处。
    let cases: Vec<&Case> = all
        .iter()
        .filter(|case| !case.context.is_empty() && !case.category.starts_with("clause-"))
        .collect();
    let mut same_key: HashMap<&str, Vec<&Case>> = HashMap::new();
    for &case in &cases {
        same_key.entry(case.input).or_default().push(case);
    }

    let mut rerank = Rerankers {
        four: Rerank::new(&models.four),
        five: Rerank::new(&models.five),
    };
    let mut targets = 0;
    let mut top1 = [0usize; ROUNDS];
    let mut top5 = [0usize; ROUNDS];
    let mut guarded = 0;
    let mut broken = 0;
    let mut stuck = Vec::new();
    let step = (cases.len() / SCAN).max(1);
    for case in cases.iter().step_by(step) {
        if targets == TARGETS {
            break;
        }
        let expected = bench::normalize(case.expected);
        let mut engine = bench::make_engine(dict, gram, Fuzzy::auto());
        if rank(&mut engine, &mut rerank, case, &expected) <= 0 {
            continue;
        }
        // 学习前先记下同键对照里原本正确的用例。
        let siblings: Vec<(&Case, String)> = same_key[case.input]
            .iter()
            .map(|other| (*other, bench::normalize(other.expected)))
            .filter(|(_, gold)| *gold != expected)
            .filter(|(other, gold)| {
                let mut fresh = bench::make_engine(dict, gram, Fuzzy::auto());
                rank(&mut fresh, &mut rerank, other, gold) == 0
            })
            .collect();

        // 每轮先选中,再量名次;组合在量完后保留,下一轮直接选。
        let mut ranks = [-1; ROUNDS];
        let mut learned = true;
        for slot in ranks.iter_mut() {
            if !choose(&mut engine, &expected) {
                learned = false;
                break;
            }
            *slot = rank(&mut engine, &mut rerank, case, &expected);
        }
        if !learned {
            continue;
        }
        targets += 1;
        for (round, slot) in ranks.iter().enumerate() {
            top1[round] += usize::from(*slot == 0);
            top5[round] += usize::from((0..5).contains(slot));
        }
        if ranks[ROUNDS - 1] != 0 {
            stuck.push(format!(
                "{} / {} → {} 名次 {:?}",
                case.context, case.input, case.expected, ranks,
            ));
        }
        for (other, gold) in &siblings {
            guarded += 1;
            broken += usize::from(rank(&mut engine, &mut rerank, other, gold) != 0);
        }
    }

    let _ = writeln!(
        report,
        "\n## 学习基准\n\n目标 {targets} 条,同键对照 {guarded} 条。\n\n| 选中次数 | 首选 | 前五 |\n|---|---:|---:|",
    );
    for (round, (&one, &five)) in top1.iter().zip(&top5).enumerate() {
        let _ = writeln!(
            report,
            "| {} | {:.1}% | {:.1}% |",
            round + 1,
            rate(one, targets),
            rate(five, targets),
        );
    }
    let _ = writeln!(
        report,
        "\n同键误伤 {broken} / {guarded}({:.1}%)",
        rate(broken, guarded)
    );
    if !stuck.is_empty() {
        let _ = writeln!(report, "\n选满 {ROUNDS} 次仍不是首选 {} 条:\n", stuck.len());
        for line in stuck.iter().take(STUCK_SHOWN) {
            let _ = writeln!(report, "- {line}");
        }
    }
    usize::from(targets == 0)
}
