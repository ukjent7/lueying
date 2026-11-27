//! 失败归因(青简式):把"正确答案丢在哪一步"分档统计,而不是只看一个 P@1。
//!
//! 每一句按产品链路的顺序走,逐档记录名次:解码候选池 → 8M 同步门控 → 25M 停顿重排。
//! 首选错的那句按下面四类归档,互斥且能指到改哪一层:
//!
//! - `未入池`:候选池里连这个文本都没有(解码器的束就没走到这条分段);
//! - `解码未组出`:池里有它的严格前缀(首词在),但没有一条候选拼到整句——差在组句深度;
//! - `重排未顶上`:池里有、名次靠后,门控要么没选它,要么选了别人;
//! - `门控拦下`:模型确实选了它,但 `promote` 拒了(整串候选/保护位/被更明确的读法支配等)。
//!
//! 用法:`lueying-bench attribution --dict <主词典> --rerank-dir <模型目录>
//!  [--gram <搭配表>] [--corpus <语料>] [--cases N] [--by-category]`

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use lueying_core::{Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank;
use crate::shared;

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Stage {
    Hit,
    Missing,
    Depth,
    Ranked,
    Gated,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Stage::Hit => "首选正确",
            Stage::Missing => "未入池",
            Stage::Depth => "解码未组出",
            Stage::Ranked => "重排未顶上",
            Stage::Gated => "门控拦下",
        }
    }
}

/// 池里有没有这个文本;没有时看有没有它的严格前缀(判定是"没走到深度"还是"根本没进池")。
fn pool_stage(pool: &[String], expected: &str) -> (Stage, i64) {
    if let Some(rank) = pool
        .iter()
        .position(|text| bench::matches_normalized(text, expected))
    {
        return (Stage::Ranked, rank as i64);
    }
    let prefix = pool.iter().any(|text| {
        let chars: Vec<char> = expected.chars().collect();
        let head: String = chars.iter().take(text.chars().count()).collect();
        text.chars().count() < chars.len() && bench::matches_normalized(text, &head)
    });
    (if prefix { Stage::Depth } else { Stage::Missing }, -1)
}

pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(rerank_dir)) = (value(args, "--dict"), value(args, "--rerank-dir"))
    else {
        eprintln!("attribution 需要 --dict 与 --rerank-dir(可选 --gram、--corpus、--cases)");
        return 2;
    };
    let corpus_path = value(args, "--corpus").unwrap_or("dicts/eval-dev-v4.tsv");
    let limit = value(args, "--cases")
        .and_then(|text| text.parse().ok())
        .unwrap_or(usize::MAX);
    let by_category = args.iter().any(|a| a == "--by-category");
    // `--only 8m` / `--only 25m` 只跑一档:用来量"多出来的容量到底值不值",也是蒸馏前的门槛数据。
    let only = value(args, "--only").unwrap_or("both").to_string();
    let use_sync = only != "25m";
    let use_settle = only != "8m";
    let Ok((dict, _)) = Dictionary::from_path(Path::new(dict_path)) else {
        eprintln!("词典加载失败 {dict_path}");
        return 2;
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let Ok(models) = rerank::load(Path::new(rerank_dir)) else {
        eprintln!("模型加载失败 {rerank_dir}");
        return 2;
    };
    let Ok(body) = std::fs::read_to_string(corpus_path) else {
        eprintln!("读语料失败 {corpus_path}");
        return 2;
    };
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let (cases, malformed) = shared::cases(leaked);
    let dict = Arc::new(dict);
    let mut four = Rerank::new(&models.four);
    let mut five = Rerank::new(&models.five);

    let mut rows = String::from(
        "category\tinput\texpected\tpool_rank\trank_8m\trank_settled\tstage\tdecide_8m\tpromote_8m\tdecide_25m\tpromote_25m\n",
    );
    let mut totals: Vec<(Stage, usize)> = Vec::new();
    let mut per_category: std::collections::BTreeMap<&str, Vec<(Stage, usize)>> =
        std::collections::BTreeMap::new();
    let mut counted = 0usize;
    for case in cases.iter().take(limit) {
        let expected = bench::normalize(case.expected);
        let mut engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
        engine.set_learning(false);
        engine.set_context(case.context);
        for c in case.input.chars() {
            engine.process(Key::Letter(c));
        }
        let pool: Vec<String> = engine
            .candidates_detailed()
            .iter()
            .filter(|cand| !cand.quick)
            .map(|cand| cand.text.clone())
            .collect();
        let (mut stage, pool_rank) = pool_stage(&pool, &expected);
        let mut decide_8m = String::from("-");
        let mut promote_8m = String::from("-");
        let mut decide_25m = String::from("-");
        let mut promote_25m = String::from("-");
        // 8M 同步门控。
        let snap = rerank::snapshot(&engine);
        if use_sync && !snap.context.is_empty() && !snap.texts.is_empty() {
            let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
            if let Some(index) = four.decide(&snap.context, &texts, &snap.facts) {
                let picked = engine
                    .candidates_detailed()
                    .get(index)
                    .map(|cand| cand.text.clone())
                    .unwrap_or_default();
                decide_8m = picked.clone();
                let ok = engine.promote(index);
                promote_8m = if ok { "ok" } else { "拒" }.to_string();
                if ok && stage == Stage::Ranked && bench::matches_normalized(&picked, &expected) {
                    stage = Stage::Hit;
                }
                if !ok && bench::matches_normalized(&picked, &expected) {
                    stage = Stage::Gated;
                }
            }
        }
        let rank_8m = shared::rank_of(&engine, &expected);
        // 25M 停顿重排。
        let snap = rerank::snapshot(&engine);
        if use_settle && !snap.context.is_empty() && !snap.texts.is_empty() {
            let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
            if let Some(index) = five.decide_settled(&snap.context, &texts, &snap.facts) {
                let picked = engine
                    .candidates_detailed()
                    .get(index)
                    .map(|cand| cand.text.clone())
                    .unwrap_or_default();
                decide_25m = picked.clone();
                let ok = engine.promote(index);
                promote_25m = if ok { "ok" } else { "拒" }.to_string();
                if ok && stage == Stage::Ranked && bench::matches_normalized(&picked, &expected) {
                    stage = Stage::Hit;
                }
                if !ok && bench::matches_normalized(&picked, &expected) {
                    stage = Stage::Gated;
                }
            }
        }
        let rank_settled = shared::rank_of(&engine, &expected);
        if rank_settled == 0 {
            stage = Stage::Hit;
        }
        // 「重排未顶上」再细分:模型自己在候选里更看好期望,却没人让它换位——
        // 那一档才是门控(整串保护/领先幅度/读法支配)拦下的,其余是真的没看出来。
        if stage == Stage::Ranked {
            let texts = engine
                .candidates_detailed()
                .iter()
                .map(|cand| cand.text.clone())
                .collect::<Vec<_>>();
            if let (Some(at), Some(leader)) = (
                texts
                    .iter()
                    .position(|text| bench::matches_normalized(text, &expected)),
                texts.first().cloned(),
            ) {
                let scores = four.scores(&snap.context, &[leader.as_str(), texts[at].as_str()]);
                if scores.len() == 2 && scores[1] > scores[0] {
                    stage = Stage::Gated;
                }
            }
        }
        let bump = |list: &mut Vec<(Stage, usize)>| {
            if let Some(entry) = list.iter_mut().find(|(seen, _)| *seen == stage) {
                entry.1 += 1;
            } else {
                list.push((stage, 1));
            }
        };
        bump(&mut totals);
        bump(per_category.entry(case.category).or_default());
        counted += 1;
        let _ = writeln!(
            rows,
            "{}\t{}\t{}\t{pool_rank}\t{rank_8m}\t{rank_settled}\t{}\t{decide_8m}\t{promote_8m}\t{decide_25m}\t{promote_25m}",
            case.category,
            case.input.replace(['\t', '\n'], " "),
            case.expected.replace(['\t', '\n'], " "),
            stage.label()
        );
    }
    let order = [
        Stage::Hit,
        Stage::Missing,
        Stage::Depth,
        Stage::Ranked,
        Stage::Gated,
    ];
    let share = |list: &[(Stage, usize)], stage: Stage, total: usize| {
        list.iter()
            .find(|(seen, _)| *seen == stage)
            .map_or(0.0, |(_, n)| 100.0 * *n as f64 / total.max(1) as f64)
    };
    let mut out = format!(
        "# 失败归因({corpus_path},--only {only})\n\n{counted} 句(坏行 {malformed}),学习关闭。\n\n| 阶段 | 句数 | 占比 |\n|---|---:|---:|\n"
    );
    for stage in order {
        let n = totals
            .iter()
            .find(|(seen, _)| *seen == stage)
            .map_or(0, |(_, n)| *n);
        let _ = writeln!(
            out,
            "| {} | {n} | {:.2}% |",
            stage.label(),
            share(&totals, stage, counted)
        );
    }
    if by_category {
        out.push_str("\n## 分类别\n\n| 类别 | 句数 | 首选正确 | 未入池 | 解码未组出 | 重排未顶上 | 门控拦下 |\n|---|---:|---:|---:|---:|---:|---:|\n");
        for (category, list) in &per_category {
            let total: usize = list.iter().map(|(_, n)| n).sum();
            let _ = writeln!(
                out,
                "| {category} | {total} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% |",
                share(list, Stage::Hit, total),
                share(list, Stage::Missing, total),
                share(list, Stage::Depth, total),
                share(list, Stage::Ranked, total),
                share(list, Stage::Gated, total)
            );
        }
    }
    print!("{out}");
    if std::fs::create_dir_all("target/bench").is_ok() {
        let _ = std::fs::write("target/bench/attribution.tsv", rows);
    }
    0
}
