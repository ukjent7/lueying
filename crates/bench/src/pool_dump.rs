//! 重排比选池转储:每条用例把「引擎看到的候选快照 + 两档模型的逐字对数概率」
//! 落成一行 JSON,供离线模拟各种「窗口选哪些候选进比选」的策略,不必每改一次
//! 策略就重跑一遍推理。
//!
//! 用法:`lueying-bench pool-dump --dict <主词典> --rerank-dir <模型目录>
//!   [--gram <搭配表>] [--corpus <语料>] [--cases N] [--out <文件>]`

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use lueying_core::{Engine, Fuzzy, Key};
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

fn json_escape(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    json_escape(text, &mut out);
    out
}

fn dump_case(ctx: &Ctx, case: &shared::Case) -> String {
    let mut engine = ctx.engine();
    // 与 learn-sim 的 type_key 同口径:先 Esc 再设上文。
    engine.process(Key::Esc);
    engine.set_context(case.context);
    for c in case.input.chars() {
        engine.process(Key::Letter(c));
    }
    let snap = rerank::snapshot(&engine);
    let detailed = engine.candidates_detailed();
    let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
    let compared: Vec<usize> = (0..texts.len())
        .filter(|&index| snap.facts[index].answers_key)
        .collect();
    let pool: Vec<&str> = compared.iter().map(|&index| texts[index]).collect();
    let mut four = Rerank::new(&ctx.models.four);
    let mut five = Rerank::new(&ctx.models.five);
    let lp8 = four.scores(&snap.context, &pool);
    let lp25 = five.scores(&snap.context, &pool);
    let skip = detailed.iter().any(|cand| cand.quick || cand.pinned);
    let mut out = String::with_capacity(4096);
    let _ = write!(
        out,
        "{{\"cat\":{},\"input\":{},\"expected\":{},\"ctx\":{},\"skip\":{},\"cands\":[",
        json_str(case.category),
        json_str(case.input),
        json_str(case.expected),
        json_str(&snap.context),
        skip
    );
    for (at, cand) in detailed.iter().enumerate() {
        if at > 0 {
            out.push(',');
        }
        let fact = &snap.facts[at];
        let lp = |scores: &[f32]| -> String {
            compared
                .iter()
                .position(|&index| index == at)
                .and_then(|slot| scores.get(slot))
                .map_or("null".into(), |score| format!("{score:.6}"))
        };
        let _ = write!(
            out,
            "{{\"t\":{},\"ans\":{},\"tr\":{},\"sc\":{:.4},\"ch\":{:.4},\"per\":{:.4},\
             \"lock\":{},\"ed\":{},\"full\":{},\"qk\":{},\"pin\":{},\"rej\":{},\"nl\":{},\
             \"weak\":{},\"ncp\":{},\"att\":{},\"lp8\":{},\"lp25\":{}}}",
            json_str(&cand.text),
            u8::from(fact.answers_key),
            u8::from(fact.trusted_dictionary_hit),
            fact.score,
            fact.channel,
            fact.personal,
            u8::from(fact.locked),
            u8::from(fact.edited),
            u8::from(cand.full),
            u8::from(cand.quick),
            u8::from(cand.pinned),
            u8::from(cand.rejected),
            u8::from(cand.no_learn),
            u8::from(cand.weaker_reading),
            u8::from(cand.needs_clause_prefix),
            u8::from(cand.attested_context),
            lp(&lp8),
            lp(&lp25),
        );
    }
    out.push_str("]}");
    out
}

struct Ctx {
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: rerank::Models,
}

impl Ctx {
    fn engine(&self) -> Engine {
        let mut engine = bench::make_engine(&self.dict, &self.gram, Fuzzy::auto());
        engine.set_learning(false);
        engine
    }
}

pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(rerank_dir)) = (value(args, "--dict"), value(args, "--rerank-dir"))
    else {
        eprintln!("pool-dump 需要 --dict 与 --rerank-dir(可选 --gram、--corpus、--cases、--out)");
        return 2;
    };
    let corpus_path = value(args, "--corpus").unwrap_or("dicts/eval-dev-v4.tsv");
    let limit = value(args, "--cases")
        .and_then(|text| text.parse().ok())
        .unwrap_or(usize::MAX);
    let out_path = value(args, "--out").unwrap_or("target/pool-dump.jsonl");
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
    let cases: Vec<shared::Case> = cases.into_iter().take(limit).collect();
    let ctx = Arc::new(Ctx {
        dict: Arc::new(dict),
        gram,
        models,
    });
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .max(1);
    let chunk = cases.len().div_ceil(threads).max(1);
    let mut lines: Vec<String> = vec![String::new(); cases.len()];
    std::thread::scope(|scope| {
        let mut spans = lines.as_mut_slice();
        let mut rest_cases = cases.as_slice();
        let mut handles = Vec::new();
        while !rest_cases.is_empty() {
            let take = rest_cases.len().min(chunk);
            let (part_cases, tail_cases) = rest_cases.split_at(take);
            let (part_lines, tail_lines) = spans.split_at_mut(take);
            rest_cases = tail_cases;
            spans = tail_lines;
            let ctx = Arc::clone(&ctx);
            handles.push(scope.spawn(move || {
                for (case, line) in part_cases.iter().zip(part_lines.iter_mut()) {
                    *line = dump_case(&ctx, case);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("pool-dump 分片线程");
        }
    });
    let mut text = String::new();
    for line in &lines {
        text.push_str(line);
        text.push('\n');
    }
    match std::fs::write(out_path, text) {
        Ok(()) => {
            eprintln!("pool-dump: {} 句(坏行 {malformed}) → {out_path}", lines.len());
            0
        }
        Err(error) => {
            eprintln!("写 {out_path} 失败: {error}");
            2
        }
    }
}
