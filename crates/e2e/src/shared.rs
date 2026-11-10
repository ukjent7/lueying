//! 共享语料基准:同一份冻结语料、同一套归一化,只产出逐条结果。
//!
//! 结果写到 `target/bench/<语料>.<分片>.tsv`;指标、门限与显著性都由
//! `tools/bench_shared.py` 从这份数据算,这里不做汇总,这样分片和跨引擎结果走同一条路。

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use lueying_core::{Candidate, Engine, Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::Models;

const CORPUS: &str = include_str!("../../../dicts/eval-shared-v1.tsv");
const CORPUS_NAME: &str = "eval-shared-v1";
const SCHEMA: &str = "lueying-bench-v1";
const COLUMNS: &str = "category\tinput\texpected\trank\ttop1\tms\t\
    rank_8m\trank_settled\ttop1_settled\tpool_rank\tpool_len\tms_8m\tms_25m\tpicks\tpages";
/// 逐段选词的上限,足够覆盖语料里最长的句子。
const MAX_PICKS: usize = 16;
/// 特征导出的候选数:短输入看得更深,整句只看前面几条。
const FEATURE_CANDS_SHORT: usize = 32;
const FEATURE_CANDS_LONG: usize = 12;
const FEATURE_SHORT_BYTES: usize = 12;

pub(crate) struct Case<'a> {
    pub(crate) category: &'a str,
    pub(crate) input: &'a str,
    pub(crate) expected: &'a str,
    pub(crate) context: &'a str,
}

pub(crate) fn cases(corpus: &'static str) -> (Vec<Case<'static>>, usize) {
    let mut rows = Vec::new();
    let mut malformed = 0;
    for line in corpus.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(category), Some(_source), Some(input), Some(expected)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            malformed += 1;
            continue;
        };
        let context = fields.next().unwrap_or("");
        rows.push(Case {
            category,
            input,
            expected,
            context,
        });
    }
    (rows, malformed)
}

fn escape(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

/// 与 `bench::eval_texts` 同口径的候选排名:快捷项与固定项不占名额。
pub(crate) fn rank_of(engine: &Engine, expected: &str) -> i64 {
    bench::eval_texts(engine)
        .into_iter()
        .position(|text| bench::matches_normalized(text, expected))
        .map_or(-1, |rank| rank as i64)
}

pub(crate) fn pct(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[((p / 100.0) * (values.len() - 1) as f64).round() as usize]
}

/// 模拟用户把句子打出来的成本:每次选能接上剩余文字的最长候选,需要时翻页。
/// 返回选词次数与翻页次数;打不出来时选词次数为 -1。
fn input_cost(engine: &mut Engine, expected: &str) -> (i64, i64) {
    let size = engine.page_size();
    let mut rest = expected.to_string();
    let (mut picks, mut pages) = (0, 0);
    for _ in 0..MAX_PICKS {
        let best = engine
            .candidates()
            .iter()
            .enumerate()
            .map(|(at, text)| (at, bench::normalize(text)))
            .filter(|(_, text)| !text.is_empty() && rest.starts_with(text.as_str()))
            .max_by_key(|(at, text)| (text.chars().count(), std::cmp::Reverse(*at)));
        let Some((at, text)) = best else {
            return (-1, pages);
        };
        for _ in 0..at / size {
            engine.process(Key::PageDown);
        }
        pages += (at / size) as i64;
        let Ok(digit) = u8::try_from(at % size + 1) else {
            return (-1, pages);
        };
        engine.process(Key::Digit(digit));
        picks += 1;
        rest = rest[text.len()..].to_string();
        if rest.is_empty() {
            return (picks, pages);
        }
        if engine.composition().is_none() {
            return (-1, pages);
        }
    }
    (-1, pages)
}

/// 一条用例的逐条结果,列序同 `COLUMNS`。
struct Outcome {
    rank: i64,
    top1: String,
    ms: f64,
    rank_8m: i64,
    rank_settled: i64,
    top1_settled: String,
    pool_rank: i64,
    pool_len: usize,
    ms_8m: f64,
    ms_25m: f64,
    picks: i64,
    pages: i64,
    /// 重排前每条整键候选的特征行,离线拟合重排权重用。
    features: Vec<String>,
}

/// 还原产品链路:8M 逐键同步、25M 停顿。快捷与置顶候选的位置由插入顺序决定,
/// 产品直接跳过重排,这里也一样。返回 8M 名次与两个模型的耗时。
pub(crate) fn apply_rerank(
    engine: &mut Engine,
    four: &mut Rerank,
    five: &mut Rerank,
    expected: &str,
) -> (i64, f64, f64) {
    let skip = engine
        .candidates_detailed()
        .iter()
        .any(|cand| cand.quick || cand.pinned);
    if skip {
        return (rank_of(engine, expected), 0.0, 0.0);
    }
    let snap = crate::rerank::snapshot(engine);
    let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
    let started = Instant::now();
    // 与 `lueying_core::session::sync_worthwhile` 同一门控:没有上文就跳过。
    let promote = (!snap.context.is_empty())
        .then(|| four.decide(&snap.context, &texts, &snap.facts))
        .flatten();
    let ms_8m = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(index) = promote {
        engine.promote(index);
    }
    let rank_8m = rank_of(engine, expected);

    let snap = crate::rerank::snapshot(engine);
    let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
    let started = Instant::now();
    let promote = five.decide_settled(&snap.context, &texts, &snap.facts);
    let ms_25m = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(index) = promote {
        engine.promote(index);
    }
    (rank_8m, ms_8m, ms_25m)
}

/// 一片用例的评测;每片自带引擎与模型句柄,片间无共享可变状态。
fn run_chunk(
    chunk: &[&Case],
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    models: Option<&Models>,
    features: bool,
) -> Vec<Outcome> {
    let mut engine = bench::make_engine(dict, gram, Fuzzy::auto());
    // 成本模拟要真的选词,不关学习的话前面的用例会抬高后面的答案。
    engine.set_learning(false);
    let mut four = models.map(|m| Rerank::new(&m.four));
    let mut five = models.map(|m| Rerank::new(&m.five));
    let mut out = Vec::with_capacity(chunk.len());
    for case in chunk {
        engine.process(Key::Esc);
        if case.context.is_empty() {
            engine.clear_context();
        } else {
            engine.set_context(case.context);
        }
        let started = Instant::now();
        for c in case.input.chars() {
            engine.process(Key::Letter(c));
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let expected = bench::normalize(case.expected);
        let rank = rank_of(&engine, &expected);
        let top1 = bench::eval_texts(&engine)
            .first()
            .map_or(String::new(), |text| (*text).to_string());
        // 重排池:只留整键候选,即重排真正能比较的那部分。
        let pool: Vec<&str> = engine
            .candidates_detailed()
            .iter()
            .filter(|cand| !cand.no_learn && !cand.quick && cand.full)
            .map(|cand| cand.text.as_str())
            .collect();
        let pool_rank = pool
            .iter()
            .position(|text| bench::matches_normalized(text, &expected))
            .map_or(-1, |at| at as i64);
        let pool_len = pool.len();
        let limit = if case.input.len() <= FEATURE_SHORT_BYTES {
            FEATURE_CANDS_SHORT
        } else {
            FEATURE_CANDS_LONG
        };
        let listed: Vec<(usize, Candidate)> = engine
            .candidates_detailed()
            .iter()
            .enumerate()
            .filter(|(_, cand)| !cand.no_learn && !cand.quick && cand.full)
            .take(limit)
            .map(|(at, cand)| (at, cand.clone()))
            .collect();
        let context = crate::rerank::snapshot(&engine).context;
        let (rank_8m, ms_8m, ms_25m) = match (four.as_mut(), five.as_mut()) {
            (Some(four), Some(five)) => apply_rerank(&mut engine, four, five, &expected),
            _ => (rank, 0.0, 0.0),
        };
        let top1_settled = bench::eval_texts(&engine)
            .first()
            .map_or(String::new(), |text| (*text).to_string());
        let rank_settled = rank_of(&engine, &expected);
        let (picks, pages) = input_cost(&mut engine, &expected);
        let features = match (four.as_mut(), five.as_mut()) {
            (Some(four), Some(five)) if features => {
                feature_rows(case, &expected, &context, &listed, four, five)
            }
            _ => Vec::new(),
        };
        out.push(Outcome {
            rank,
            top1,
            ms,
            rank_8m,
            rank_settled,
            top1_settled,
            pool_rank,
            pool_len,
            ms_8m,
            ms_25m,
            picks,
            pages,
            features,
        });
    }
    out
}

/// 每条候选一行:引擎给的事实加两个模型的每字平均对数概率。
fn feature_rows(
    case: &Case,
    expected: &str,
    context: &str,
    listed: &[(usize, Candidate)],
    four: &mut Rerank,
    five: &mut Rerank,
) -> Vec<String> {
    let texts: Vec<&str> = listed.iter().map(|(_, cand)| cand.text.as_str()).collect();
    let (lm8, lm25) = (four.scores(context, &texts), five.scores(context, &texts));
    listed
        .iter()
        .zip(lm8.iter().zip(&lm25))
        .map(|((at, cand), (lm8, lm25))| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.4}\t{}",
                case.category,
                escape(case.input),
                escape(expected),
                at,
                escape(&cand.text),
                cand.score,
                cand.channel,
                cand.edits,
                u8::from(cand.trusted),
                u8::from(cand.sentence),
                u8::from(cand.correction),
                u8::from(cand.completion),
                cand.freq,
                cand.words.len(),
                lm8,
                lm25,
                context.chars().count(),
            )
        })
        .collect()
}

/// 跑一份语料的第 `shard.0` 片(共 `shard.1` 片),返回失败数。
pub fn run(
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: Option<&Models>,
    corpus: Option<&str>,
    shard: (usize, usize),
) -> usize {
    // 换用的语料只活到进程结束,泄漏掉省得 Case 带生命周期穿过线程。
    let (body, name) = match corpus {
        Some(path) => {
            let body = std::fs::read_to_string(path).expect("读语料");
            let name = std::path::Path::new(path)
                .file_stem()
                .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
            let body: &'static str = Box::leak(body.into_boxed_str());
            (body, name)
        }
        None => (CORPUS, CORPUS_NAME.to_string()),
    };
    let (cases, malformed) = cases(body);
    let (at, total) = shard;
    let picked: Vec<&Case> = cases.iter().skip(at).step_by(total.max(1)).collect();

    // 用例互不依赖,按核数分片并行。模型权重在 Arc 后共享,每线程只多一份轻量句柄。
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .max(1);
    let chunk_size = picked.len().div_ceil(threads).max(1);
    // 私有语料的文字不能离开加密文件,不导出特征。
    let features = !name.starts_with("eval-private");
    let dict_ref = &dict;
    let gram_ref = &gram;
    let outcomes: Vec<Vec<Outcome>> = std::thread::scope(|scope| {
        let handles: Vec<_> = picked
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || run_chunk(chunk, dict_ref, gram_ref, models, features))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("共享语料基准分片线程"))
            .collect()
    });

    let mut tsv = String::new();
    let mut feat = String::new();
    let _ = writeln!(tsv, "# schema={SCHEMA}");
    let _ = writeln!(tsv, "# engine=lueying {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(tsv, "# corpus={name}");
    let _ = writeln!(tsv, "# kind=items\n{COLUMNS}");
    for (case, item) in picked.iter().zip(outcomes.into_iter().flatten()) {
        for row in &item.features {
            feat.push_str(row);
            feat.push('\n');
        }
        let _ = writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{}\t{}",
            case.category,
            escape(case.input),
            escape(case.expected),
            item.rank,
            escape(&item.top1),
            item.ms,
            item.rank_8m,
            item.rank_settled,
            escape(&item.top1_settled),
            item.pool_rank,
            item.pool_len,
            item.ms_8m,
            item.ms_25m,
            item.picks,
            item.pages,
        );
    }
    let path = format!("target/bench/{name}.{at}.tsv");
    let written = std::fs::create_dir_all("target/bench")
        .and_then(|()| std::fs::write(&path, tsv))
        .map_err(|error| eprintln!("写 {path} 失败: {error}"))
        .is_ok();
    if features {
        let _ = std::fs::write(format!("target/bench/{name}.{at}.feat"), feat);
    }
    println!("{path}: {} 条", picked.len());
    usize::from(malformed > 0) + usize::from(picked.is_empty()) + usize::from(!written)
}
