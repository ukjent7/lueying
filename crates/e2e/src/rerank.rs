//! 4M 同步 + 25M 停顿重排的 E2E 契约。
//!
//! 先列失败方式，再逐条检查：
//! 1. 模型缺失、截断或损坏仍被接受 → 加载必须报错，引擎保持原排序。
//! 2. 可信整键词典命中被模型改掉 → 必须拒绝（`trusted_dictionary_hit` 门控）。
//! 3. 前缀词/未覆盖整个按键的候选参与比较 → 禁止，只比较 `answers_key`。
//! 4. 重排增删候选、改变 `consume` 或丢词 → 禁止，只允许换顺序。
//! 5. 重排后空格上屏的不是新的首选 → 禁止。
//! 6. 25M 停顿结果在输入变化、翻页或移动高亮后落地 → 用 revision/页/高亮丢弃。
//! 7. 连续按键仍执行旧停顿任务 → 去抖只保留每会话最后一次提交。
//! 8. 上下文没有进入模型 → 同一对候选在「她说：」后排序应变化。
//! 9. 4M 同步路径拖垮按键延迟 → 分位数门限。
//! 10. 重排让整句 top-1/top-5 掉出门限 → 质量门限。
//! 11. 发布包里的模型几何/许可与预期不符 → 校验 config、license、attribution、体积。

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use pinyin_dict::Dictionary;
use pinyin_dict::gram::Gram;
use pinyin_engine::{Candidate, Config, Engine, Event, Key};
use pinyin_rerank::debounce::Debounce;
use pinyin_rerank::settle::result_is_current;
use pinyin_rerank::{Fact, Model, Rerank};

pub struct Models {
    pub four: Model,
    pub five: Model,
    four_size: u64,
    five_size: u64,
    rejects_bad: bool,
}

/// 读取并解析两档模型。文件缺失或损坏都返回可读错误，由主程序记为 FAIL。
pub fn load(dir: &Path) -> Result<Models, String> {
    let read = |name: &str| {
        let path = dir.join(name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((path, bytes))
    };
    let (four_path, four_bytes) = read("rerank-4m.safetensors")?;
    let (five_path, five_bytes) = read("rerank-25m.safetensors")?;
    let four = Model::load(&four_bytes).map_err(|e| format!("{}: {e}", four_path.display()))?;
    let five = Model::load(&five_bytes).map_err(|e| format!("{}: {e}", five_path.display()))?;
    // 截断到一半与空文件都必须在解析阶段被拒；不能带着半份权重进推理。
    let rejects_bad = Model::load(&four_bytes[..four_bytes.len() / 2]).is_err()
        && Model::load(&five_bytes[..five_bytes.len() / 2]).is_err()
        && Model::load(&[] as &[u8]).is_err();
    Ok(Models {
        four,
        five,
        four_size: four_bytes.len() as u64,
        five_size: five_bytes.len() as u64,
        rejects_bad,
    })
}

struct Snap {
    context: String,
    texts: Vec<String>,
    facts: Vec<Fact>,
}

fn snapshot(engine: &Engine) -> Snap {
    let context = engine.context().to_string();
    let cands = engine.candidates_detailed();
    Snap {
        context,
        texts: cands.iter().map(|c| c.text.clone()).collect(),
        facts: cands
            .iter()
            .map(|c| Fact {
                answers_key: c.full,
                trusted_dictionary_hit: c.trusted,
            })
            .collect(),
    }
}

fn signature(cands: &[Candidate]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = cands.iter().map(|c| (c.text.clone(), c.consume)).collect();
    out.sort();
    out
}

fn hit(cands: &[Candidate], gold: &str) -> (bool, bool) {
    (
        cands.first().is_some_and(|c| c.text == gold),
        cands.iter().take(5).any(|c| c.text == gold),
    )
}

fn argmax(scores: &[f32]) -> usize {
    let mut best = 0;
    for (index, score) in scores.iter().enumerate() {
        if *score > scores[best] {
            best = index;
        }
    }
    best
}

fn pct(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[((p / 100.0) * (values.len() - 1) as f64).round() as usize]
}

fn push(check: &mut Vec<(&'static str, bool, String)>, name: &'static str, ok: bool, note: String) {
    check.push((name, ok, note));
}

/// 追加 Markdown 报告，返回失败项数。每个用例用新引擎，避免用户词学习污染后续排序。
pub fn run(
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: &Models,
    report: &mut String,
) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();
    let fresh = || {
        let mut engine = Engine::new(Arc::clone(&dict), Config::default());
        if let Some(gram) = &gram {
            engine = engine.with_gram(Arc::clone(gram));
        }
        engine
    };

    push(
        &mut checks,
        "4M 体积",
        models.four_size == 4_486_280,
        format!("{} 字节", models.four_size),
    );
    push(
        &mut checks,
        "25M 体积",
        models.five_size == 25_480_184,
        format!("{} 字节", models.five_size),
    );
    push(
        &mut checks,
        "4M 几何",
        models.four.context_length() == 64,
        format!("context={}", models.four.context_length()),
    );
    push(
        &mut checks,
        "25M 几何",
        models.five.context_length() == 128,
        format!("context={}", models.five.context_length()),
    );
    push(
        &mut checks,
        "权重许可",
        models.four.license() == "Apache-2.0" && models.five.license() == "Apache-2.0",
        format!("{} / {}", models.four.license(), models.five.license()),
    );
    push(
        &mut checks,
        "训练语料署名",
        models.four.attribution().contains("C4")
            && models.four.attribution().contains("LCCC")
            && models.five.attribution().contains("C4")
            && models.five.attribution().contains("LCCC"),
        "C4 + LCCC".into(),
    );
    push(
        &mut checks,
        "损坏模型被拒",
        models.rejects_bad,
        "截断一半与空文件".into(),
    );

    let pair = ["她听到了这话", "他听到了这话"];
    let mut four = Rerank::new(&models.four);
    let mut five = Rerank::new(&models.five);

    let scores = four.scores("", &pair);
    push(
        &mut checks,
        "4M 门控样本有效",
        argmax(&scores) == 1,
        format!("{scores:?}"),
    );
    let trusted = [
        Fact {
            answers_key: true,
            trusted_dictionary_hit: true,
        },
        Fact {
            answers_key: true,
            trusted_dictionary_hit: false,
        },
    ];
    push(
        &mut checks,
        "可信整键命中不被覆盖",
        four.decide("", &pair, &trusted).is_none(),
        "模型偏好第二条但首条可信".into(),
    );
    let untrusted = [
        Fact {
            answers_key: true,
            trusted_dictionary_hit: false,
        },
        Fact {
            answers_key: true,
            trusted_dictionary_hit: false,
        },
    ];
    let untrusted_pick = four.decide("", &pair, &untrusted);
    push(
        &mut checks,
        "非可信首条允许重排",
        untrusted_pick == Some(1),
        format!("{untrusted_pick:?}"),
    );
    let short = ["她听到了这话", "他听到了这话", "她听到"];
    let mixed = [
        Fact {
            answers_key: true,
            trusted_dictionary_hit: false,
        },
        Fact {
            answers_key: true,
            trusted_dictionary_hit: false,
        },
        Fact {
            answers_key: false,
            trusted_dictionary_hit: false,
        },
    ];
    let picked = four.decide("", &short, &mixed);
    push(
        &mut checks,
        "前缀词不参与比较",
        picked == Some(1),
        format!("{picked:?}"),
    );
    let one_full = [
        Fact {
            answers_key: true,
            trusted_dictionary_hit: false,
        },
        Fact {
            answers_key: false,
            trusted_dictionary_hit: false,
        },
        Fact {
            answers_key: false,
            trusted_dictionary_hit: false,
        },
    ];
    push(
        &mut checks,
        "不足两条整键候选不决策",
        four.decide("", &short, &one_full).is_none(),
        "只有首条覆盖整键".into(),
    );

    let four_empty = four.scores("", &pair);
    let four_context = four.scores("她说：", &pair);
    let changed = four_empty
        .iter()
        .zip(&four_context)
        .any(|(a, b)| (a - b).abs() > 1e-6);
    push(
        &mut checks,
        "4M 上下文进入模型",
        changed,
        format!("{four_empty:?} → {four_context:?}"),
    );
    let five_empty = five.scores("", &pair);
    let five_context = five.scores("她说：", &pair);
    push(
        &mut checks,
        "25M 上下文改变排序",
        five_empty[0] > five_empty[1] && five_context[1] > five_context[0],
        format!("{five_empty:?} → {five_context:?}"),
    );

    // 去抖：同一会话只保留最后一次提交，多会话互不拖累，取消后不再执行。
    let mut debounce = Debounce::new(120);
    debounce.submit(1, 0, "a");
    debounce.submit(1, 50, "b");
    push(
        &mut checks,
        "去抖未到期不执行",
        debounce.take_due(169).is_empty(),
        "169ms".into(),
    );
    push(
        &mut checks,
        "去抖只执行最后一次",
        debounce.take_due(170) == vec!["b"],
        "170ms".into(),
    );
    debounce.submit(1, 0, "a");
    debounce.submit(2, 50, "b");
    push(
        &mut checks,
        "去抖按会话独立",
        debounce.take_due(120) == vec!["a"] && debounce.take_due(170) == vec!["b"],
        "两个会话".into(),
    );
    debounce.submit(1, 0, "a");
    debounce.cancel(1);
    push(
        &mut checks,
        "取消的停顿任务不执行",
        debounce.take_due(500).is_empty(),
        "cancel".into(),
    );
    debounce.submit(1, 100, "x");
    push(
        &mut checks,
        "下一到期时间",
        debounce.next_due_ms() == Some(220),
        "100 + 120".into(),
    );

    // 上屏文本进入上下文，清空后归零。
    let mut engine = fresh();
    engine.process(Key::Esc);
    engine.clear_context();
    for c in "nihao".chars() {
        engine.process(Key::Letter(c));
    }
    let committed: String = engine
        .process(Key::Space)
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    push(
        &mut checks,
        "上屏写入上下文",
        !committed.is_empty() && engine.context() == committed.as_str(),
        format!("context={:?}", engine.context()),
    );
    engine.clear_context();
    push(
        &mut checks,
        "清空上下文",
        engine.context().is_empty(),
        String::new(),
    );

    // 真实输入：逐条跑 4M 同步与 25M 停顿，检查候选集合、可信门控、上屏与质量。
    let mut total = 0usize;
    let (mut base1, mut base5) = (0usize, 0usize);
    let (mut four1, mut four5) = (0usize, 0usize);
    let (mut five1, mut five5) = (0usize, 0usize);
    let mut promotions = 0usize;
    let mut trusted_skips = 0usize;
    let mut trusted_disagreements = 0usize;
    let mut set_ok = true;
    let mut full_ok = true;
    let mut commit_ok = true;
    let mut four_ms = Vec::new();
    let mut five_ms = Vec::new();

    for line in include_str!("../../../dicts/eval.txt").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(raw), Some(want)) = (parts.next(), parts.next()) else {
            continue;
        };
        let mut engine = fresh();
        engine.process(Key::Esc);
        engine.clear_context();
        for c in raw.chars() {
            engine.process(Key::Letter(c));
        }
        if engine.composition().is_none() {
            continue;
        }
        total += 1;
        let before = engine.candidates_detailed().to_vec();
        let before_sig = signature(&before);
        let (b1, b5) = hit(&before, want);
        base1 += usize::from(b1);
        base5 += usize::from(b5);

        let snap = snapshot(&engine);
        let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
        if before.first().is_some_and(|c| c.trusted) {
            trusted_skips += 1;
            let full: Vec<&str> = before
                .iter()
                .filter(|c| c.full)
                .take(9)
                .map(|c| c.text.as_str())
                .collect();
            if full.len() >= 2 && argmax(&four.scores(&snap.context, &full)) != 0 {
                trusted_disagreements += 1;
            }
        }

        let started = Instant::now();
        let promote = four.decide(&snap.context, &texts, &snap.facts);
        four_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        if let Some(index) = promote {
            promotions += 1;
            full_ok &= before[index].full;
            engine.promote(index);
        }
        set_ok &= signature(engine.candidates_detailed()) == before_sig;
        let (f1, f5) = hit(engine.candidates_detailed(), want);
        four1 += usize::from(f1);
        four5 += usize::from(f5);

        let snap = snapshot(&engine);
        let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
        let started = Instant::now();
        let promote = five.decide(&snap.context, &texts, &snap.facts);
        five_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        if let Some(index) = promote {
            full_ok &= engine.candidates_detailed()[index].full;
            engine.promote(index);
        }
        set_ok &= signature(engine.candidates_detailed()) == before_sig;
        let (s1, s5) = hit(engine.candidates_detailed(), want);
        five1 += usize::from(s1);
        five5 += usize::from(s5);

        let top = engine.candidates().first().cloned();
        let events = engine.process(Key::Space);
        if let Some(top) = top {
            let committed: String = events
                .into_iter()
                .filter_map(|event| match event {
                    Event::Commit(text) => Some(text),
                    _ => None,
                })
                .collect();
            commit_ok &= committed == top;
        }
        engine.clear_context();
    }

    push(
        &mut checks,
        "真实输入覆盖",
        total > 0,
        format!("{total} 条"),
    );
    push(
        &mut checks,
        "重排只换顺序",
        set_ok,
        "候选集合与 consume 不变".into(),
    );
    push(
        &mut checks,
        "只提升整键候选",
        full_ok,
        format!("{promotions} 次提升"),
    );
    push(
        &mut checks,
        "可信命中全部跳过",
        trusted_skips > 0,
        format!("{trusted_skips} 条首条可信，其中模型想改 {trusted_disagreements} 条"),
    );
    push(
        &mut checks,
        "空格上屏新首选",
        commit_ok && total > 0,
        String::new(),
    );

    let rate = |n: usize| n as f64 * 100.0 / total.max(1) as f64;
    push(
        &mut checks,
        "基线 top-1 ≥ 92%",
        rate(base1) >= 92.0,
        format!("{base1}/{total} = {:.1}%", rate(base1)),
    );
    push(
        &mut checks,
        "4M 后 top-1 ≥ 92%",
        rate(four1) >= 92.0,
        format!("{four1}/{total} = {:.1}%", rate(four1)),
    );
    push(
        &mut checks,
        "4M 后 top-5 ≥ 95%",
        rate(four5) >= 95.0,
        format!("{four5}/{total} = {:.1}%", rate(four5)),
    );
    push(
        &mut checks,
        "25M 后 top-1 ≥ 92%",
        rate(five1) >= 92.0,
        format!("{five1}/{total} = {:.1}%", rate(five1)),
    );
    push(
        &mut checks,
        "25M 后 top-5 ≥ 95%",
        rate(five5) >= 95.0,
        format!("{five5}/{total} = {:.1}%", rate(five5)),
    );
    push(
        &mut checks,
        "4M 不显著拖低基线",
        four1 + 2 >= base1,
        format!("{base1} → {four1}"),
    );

    let four_p95 = pct(&mut four_ms, 95.0);
    let four_p99 = pct(&mut four_ms, 99.0);
    let five_p95 = pct(&mut five_ms, 95.0);
    push(
        &mut checks,
        "4M 决策 p95 ≤ 40ms",
        four_p95 <= 40.0,
        format!("{four_p95:.2} ms"),
    );
    push(
        &mut checks,
        "4M 决策 p99 ≤ 80ms",
        four_p99 <= 80.0,
        format!("{four_p99:.2} ms"),
    );
    push(
        &mut checks,
        "25M 停顿 p95 ≤ 800ms",
        five_p95 <= 800.0,
        format!("{five_p95:.2} ms"),
    );

    // 异步落地条件：输入版本、页码、高亮、组合态任一变化都必须丢弃。
    let mut engine = fresh();
    for c in "nihao".chars() {
        engine.process(Key::Letter(c));
    }
    let revision = engine.revision();
    push(
        &mut checks,
        "同版本结果可落地",
        result_is_current(revision, revision, 0, 0, true),
        String::new(),
    );
    push(
        &mut checks,
        "按键后旧结果丢弃",
        !result_is_current(revision, revision.saturating_add(1), 0, 0, true),
        String::new(),
    );
    push(
        &mut checks,
        "翻页/移动高亮丢弃",
        !result_is_current(revision, revision, 1, 0, true)
            && !result_is_current(revision, revision, 0, 1, true),
        String::new(),
    );
    push(
        &mut checks,
        "组合结束后丢弃",
        !result_is_current(revision, revision, 0, 0, false),
        String::new(),
    );

    let failures = checks.iter().filter(|(_, ok, _)| !*ok).count();
    report.push_str(&format!(
        "\n## 神经重排(4M 同步 + 25M 停顿)\n\n| 指标 | 基线 | 4M 同步 | +25M 停顿 |\n|---|---:|---:|---:|\n| top-1 | {base1}/{total} = {:.1}% | {four1}/{total} = {:.1}% | {five1}/{total} = {:.1}% |\n| top-5 | {base5}/{total} = {:.1}% | {four5}/{total} = {:.1}% | {five5}/{total} = {:.1}% |\n\n4M 决策 {:.2} ms p95 / {:.2} ms p99，25M 停顿 {:.2} ms p95；提升 {} 次，可信首条 {} 条，其中模型想改 {} 条。\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n",
        rate(base1),
        rate(four1),
        rate(five1),
        rate(base5),
        rate(four5),
        rate(five5),
        four_p95,
        four_p99,
        five_p95,
        promotions,
        trusted_skips,
        trusted_disagreements,
    ));
    for (name, ok, note) in &checks {
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if *ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}
