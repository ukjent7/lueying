//! E2E 运行器:跑场景规格与主词典探针,产出可复查的 Markdown 报告并按结果设退出码。
//! 用法:`lueying-e2e [--dict 主词典.tsv] [--gram 语法模型.gram] [--bench|--bench-fast]`

mod bench;
mod rerank;
mod scenarios;
mod ui;
mod user_dict;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_engine::{Config, Engine, Event, Fuzzy, Key, Mode};
use scenarios::{Check, Scenario, probes, scenarios};

/// 二进制物化 → 重新加载 → 查询结果逐条一致;字段/查询三条路径都要对上。
fn binary_roundtrip() -> Vec<(&'static str, bool, String)> {
    let mut out = Vec::new();
    let (dict, _) = Dictionary::from_text(include_str!("../../../dicts/mini.txt"));
    let bytes = dict.to_binary();
    let reloaded = Dictionary::from_binary(&bytes);
    out.push((
        "二进制可解析",
        reloaded.is_some(),
        format!("{} 字节", bytes.len()),
    ));
    let Some(re) = reloaded else {
        return out;
    };
    out.push((
        "条数一致",
        re.len() == dict.len(),
        format!("{} / {}", re.len(), dict.len()),
    ));
    // 逐条比对:首个差异带下标与双方字段,一次 CI 就能定位问题。
    let mut diff = None;
    for (i, (a, b)) in dict.iter().zip(re.iter()).enumerate() {
        if a.word != b.word || a.key != b.key || a.freq != b.freq {
            diff = Some(format!(
                "#{i}: ({},{},{}) vs ({},{},{})",
                a.word, a.key, a.freq, b.word, b.key, b.freq
            ));
            break;
        }
    }
    out.push((
        "全部词条一致",
        diff.is_none(),
        diff.unwrap_or_else(|| format!("{} 条", dict.len())),
    ));
    out.push((
        "再导出字节稳定",
        dict.to_binary() == re.to_binary(),
        "to_binary 幂等".into(),
    ));
    let a = dict.lookup_exact("ni'hao");
    let b = re.lookup_exact("ni'hao");
    let same_entries = a.len() == b.len()
        && a.iter()
            .zip(&b)
            .all(|(x, y)| x.word == y.word && x.key == y.key && x.freq == y.freq);
    out.push((
        "精确查询一致",
        same_entries,
        format!("{} / {}", a.len(), b.len()),
    ));
    let (sa, sb) = (dict.lookup_sig("nh", 64), re.lookup_sig("nh", 64));
    out.push((
        "签名查询一致",
        sa.len() == sb.len() && sa.iter().zip(&sb).all(|(a, b)| a.word == b.word),
        format!("{} / {}", sa.len(), sb.len()),
    ));
    let (pa, pb) = (dict.lookup_prefix("ni'", 64), re.lookup_prefix("ni'", 64));
    out.push((
        "前缀查询一致",
        pa.len() == pb.len() && pa.iter().zip(&pb).all(|(a, b)| a.word == b.word),
        format!("{} / {}", pa.len(), pb.len()),
    ));
    out.push((
        "损坏文件被拒",
        Dictionary::from_binary(&bytes[..bytes.len() / 2]).is_none(),
        "截断到一半".into(),
    ));
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dict_path = args
        .iter()
        .position(|a| a == "--dict")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let gram_path = args
        .iter()
        .position(|a| a == "--gram")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let rerank_dir = args
        .iter()
        .position(|a| a == "--rerank-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let bench = if args.iter().any(|a| a == "--bench-fast") {
        Some(bench::Mode::Fast)
    } else if args.iter().any(|a| a == "--bench") {
        Some(bench::Mode::Full)
    } else {
        None
    };

    let mut report = String::from("# 拼音引擎 E2E 报告\n\n");
    let mut failures = 0usize;

    // 场景:内置迷你词库
    report.push_str("## 场景(内置迷你词库)\n\n| 场景 | 结果 | 说明 |\n|---|---|---|\n");
    for sc in scenarios() {
        match run_scenario(&sc) {
            Ok(()) => report.push_str(&format!("| {} | PASS | |\n", sc.name)),
            Err(e) => {
                failures += 1;
                report.push_str(&format!("| {} | FAIL | {e} |\n", sc.name));
            }
        }
    }

    // 二进制词典往返:布局一致才允许运行期走 mmap 路径。
    report.push_str("\n## 二进制词典往返\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    for (name, ok, note) in binary_roundtrip() {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }

    // 候选窗鼠标交互:翻页边界、点选上屏与学习。
    failures += ui::run(&mut report);

    // 用户词库:跨会话共享、导入合并、落盘文本往返。
    failures += user_dict::run(&mut report);

    // 主词典探针与性能
    let mut rerank_inputs: Option<(Arc<Dictionary>, Option<Arc<Gram>>)> = None;
    if let Some(path) = dict_path {
        let (dict, bad) = match Dictionary::from_path(&path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("词典加载失败 {path:?}: {e}");
                std::process::exit(2);
            }
        };
        report.push_str(&format!(
            "\n## 主词典探针({} 条,跳过坏行 {})\n\n| 输入 | 期望首选 | 实际首选 | 结果 |\n|---|---|---|---|\n",
            dict.len(),
            bad
        ));
        let gram = gram_path.as_deref().and_then(|p| match Gram::open(p) {
            Ok(g) => Some(Arc::new(g)),
            Err(e) => {
                eprintln!("语法模型不可用 {:?}: {e}", p);
                None
            }
        });
        let dict = Arc::new(dict);
        rerank_inputs = Some((Arc::clone(&dict), gram.clone()));
        let mut engine = Engine::new(Arc::clone(&dict), Config::default());
        if let Some(g) = &gram {
            engine = engine.with_gram(Arc::clone(g));
        }
        // 词典解析/排序的 CPU 突发会触发 runner 的 CFS 配额节流,首个键被罚站
        // 100-200ms,是测量伪影。歇一拍,再用 niha 预热三条查询路径
        // (n=签名、ni=精确、nih=前缀)后才开始计时,阈值反映稳态打字延迟。
        std::thread::sleep(std::time::Duration::from_millis(800));
        for c in "niha".chars() {
            engine.process(Key::Letter(c));
        }
        // 单次采样在共享 runner 上被调度噪声支配,门限必须看分布而不是最大值。
        // 每轮都重打全部探针:第 1 轮是冷页/冷缓存,单列报告;第 2 轮起是稳态,
        // 分位数门限只卡稳态。这样既保留「首键真实开销」这一可观察量,
        // 又不会让一次 100ms 的 CFS 罚站把整条流水线判死。
        const ROUNDS: usize = 5;
        let probes = probes();
        let mut samples: Vec<f64> = Vec::new();
        let mut cold: Vec<f64> = Vec::new();
        let mut slowest: Vec<(f64, String)> = Vec::new();
        for round in 0..ROUNDS {
            for (typed, want) in &probes {
                engine.process(Key::Esc);
                for c in typed.chars() {
                    let t = Instant::now();
                    engine.process(Key::Letter(c));
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    if round == 0 {
                        cold.push(ms);
                    } else {
                        samples.push(ms);
                        slowest.push((ms, format!("{typed}:{c}")));
                    }
                }
                if round == 0 {
                    let top = engine.candidates().first().cloned().unwrap_or_default();
                    if top == *want {
                        report.push_str(&format!("| {typed} | {want} | {top} | PASS |\n"));
                    } else {
                        failures += 1;
                        report.push_str(&format!("| {typed} | {want} | {top} | FAIL |\n"));
                    }
                }
                engine.process(Key::Esc);
            }
        }
        let count = samples.len();
        samples.sort_by(f64::total_cmp);
        let pct = |p: f64| {
            samples
                .get(((p / 100.0) * (count.max(1) - 1) as f64).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let (p50, p95, p99) = (pct(50.0), pct(95.0), pct(99.0));
        let max = samples.last().copied().unwrap_or(0.0);
        let cold_max = cold.iter().copied().fold(0.0f64, f64::max);
        slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
        let slow_report = slowest
            .iter()
            .take(5)
            .map(|(ms, k)| format!("- {k}: {ms:.2} ms"))
            .collect::<Vec<_>>()
            .join("\n");
        report.push_str(&format!(
            "\n## 性能(主词典,{count} 键 × {} 轮)\n\n| 指标 | 实测 | 门限 |\n|---|---|---|\n| 中位 | {p50:.2} ms | ≤ 4 |\n| p95 | {p95:.2} ms | ≤ 16 |\n| p99 | {p99:.2} ms | ≤ 25 |\n| 稳态最大 | {max:.2} ms | 仅诊断 |\n| 首轮最大 | {cold_max:.2} ms | 仅诊断(含冷页与调度噪声) |\n\n最慢 5 键:\n{slow_report}\n",
            ROUNDS
        ));
        // 共享 runner 的 CPU 型号与 CFS 调度差异很大:同一份代码实测 p95 可在
        // 6–12ms 之间波动。门限卡「一个 60Hz 帧」附近,抓数量级回归,不抓抖动。
        if p50 > 4.0 || p95 > 16.0 || p99 > 25.0 {
            failures += 1;
            report.push_str("\n**性能超阈值**\n");
        }

        // 整句评测:真实输入语料上量 top-1/top-5,防止解码器改动只对个别例子有效。
        let (total, top1, top5, missed) = sentence_eval(&mut engine);
        let rate = |n: usize| n as f64 * 100.0 / total.max(1) as f64;
        // 默认包不带语法模型;92% 是带语法模型时的门限,无模型按实测基线 90%。
        let top1_gate = if gram.is_some() { 92.0 } else { 90.0 };
        report.push_str(&format!(
            "\n## 整句评测(主词典,{} 条)\n\n| 指标 | 实测 | 门限 |\n|---|---|---|\n| top-1 | {top1}/{total} = {:.1}% | ≥ {top1_gate:.0}% |\n| top-5 | {top5}/{total} = {:.1}% | ≥ 95% |\n",
            if gram.is_some() { "含语法模型" } else { "无语法模型" },
            rate(top1),
            rate(top5),
        ));
        if !missed.is_empty() {
            report.push_str("\n未命中前 8 条:\n\n| 输入 | 期望 | 首选 |\n|---|---|---|\n");
            for (raw, want, got) in missed.iter().take(8) {
                report.push_str(&format!("| {raw} | {want} | {got} |\n"));
            }
        }
        if rate(top1) < top1_gate || rate(top5) < 95.0 {
            failures += 1;
            report.push_str("\n**整句准确率低于门限**\n");
        }

        // 神经重排:同一份主词典与语法模型,先基线再 4M 同步、25M 停顿。
        if let Some(dir) = &rerank_dir {
            let (dict, gram) = rerank_inputs.clone().expect("主词典刚装载");
            match rerank::load(dir) {
                Ok(models) => {
                    failures += rerank::run(dict, gram, &models, &mut report);
                }
                Err(e) => {
                    failures += 1;
                    report.push_str(&format!("\n## 神经重排\n\n模型加载失败: {e}\n"));
                }
            }
        }

        // 候选准确率基准:模糊音、语法模型、长期学习在同一份报告里对比。
        if let Some(mode) = bench {
            let (dict, gram) = rerank_inputs.clone().expect("主词典刚装载");
            failures += bench::run(dict, gram, &mut report, mode);
        }
    }

    if rerank_dir.is_some() && rerank_inputs.is_none() {
        eprintln!("--rerank-dir 需要同时提供 --dict 主词典");
        std::process::exit(2);
    }
    if bench.is_some() && rerank_inputs.is_none() {
        eprintln!("--bench / --bench-fast 需要同时提供 --dict 主词典");
        std::process::exit(2);
    }

    let out = std::path::Path::new("target/e2e-report.md");
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(out, &report);
    println!("{report}");
    println!("报告已写入 {}", out.display());
    if failures > 0 {
        println!("FAIL: {failures} 项未通过");
        std::process::exit(1);
    }
    println!("全部通过");
}

/// 整句评测:逐条打进引擎,取候选首位与 top-5。
fn sentence_eval(engine: &mut Engine) -> (usize, usize, usize, Vec<(String, String, String)>) {
    let (mut total, mut top1, mut top5) = (0usize, 0usize, 0usize);
    let mut missed = Vec::new();
    for line in include_str!("../../../dicts/eval.txt").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(raw), Some(want)) = (parts.next(), parts.next()) else {
            continue;
        };
        engine.process(Key::Esc);
        engine.clear_context();
        for c in raw.chars() {
            engine.process(Key::Letter(c));
        }
        let cands = engine.candidates();
        total += 1;
        if cands.first().is_some_and(|s| s.as_str() == want) {
            top1 += 1;
        } else {
            missed.push((
                raw.to_string(),
                want.to_string(),
                cands.first().cloned().unwrap_or_default(),
            ));
        }
        if cands.iter().take(5).any(|s| s.as_str() == want) {
            top5 += 1;
        }
    }
    (total, top1, top5, missed)
}

fn run_scenario(sc: &Scenario) -> Result<(), String> {
    let mut cfg = Config::default();
    if sc.fuzzy_off {
        cfg.fuzzy = Fuzzy::off();
    }
    let mut engine = Engine::new(std::sync::Arc::new(Dictionary::builtin()), cfg);
    engine.set_fullwidth(sc.fullwidth);
    let mut modes = vec![engine.mode()];
    for (si, stage) in sc.stages.iter().enumerate() {
        let mut commits: Vec<String> = Vec::new();
        let mut passed = false;
        for key in &stage.keys {
            for ev in engine.process(*key) {
                match ev {
                    Event::Commit(t) => commits.push(t),
                    Event::Mode(m) => modes.push(m),
                    Event::Passthrough(_) => passed = true,
                }
            }
        }
        for check in &stage.checks {
            check_one(check, &engine, &commits, &modes, passed)
                .map_err(|e| format!("阶段{}:{}", si + 1, e))?;
        }
    }
    Ok(())
}

fn check_one(
    check: &Check,
    engine: &Engine,
    commits: &[String],
    modes: &[Mode],
    passed: bool,
) -> Result<(), String> {
    let cands = engine.candidates();
    match check {
        Check::Commits(want) => {
            if commits != want.as_slice() {
                return Err(format!("上屏 {:?} ≠ 期望 {want:?}", commits));
            }
        }
        Check::NoCommits => {
            if !commits.is_empty() {
                return Err(format!("不应上屏,实际 {commits:?}"));
            }
        }
        Check::Top(w) => {
            if cands.first() != Some(w) {
                return Err(format!("首选 {:?} ≠ {w:?}", cands.first()));
            }
        }
        Check::Contains(w) => {
            if !cands.contains(w) {
                return Err(format!("候选缺 {w:?}: {cands:?}"));
            }
        }
        Check::NotContains(w) => {
            if cands.contains(w) {
                return Err(format!("候选不应含 {w:?}"));
            }
        }
        Check::Cands(want) => {
            if &cands != want {
                return Err(format!("候选 {cands:?} ≠ {want:?}"));
            }
        }
        Check::Raw(w) => {
            let raw = engine.composition().map(|c| c.raw.clone());
            if raw.as_deref() != Some(w.as_str()) {
                return Err(format!("组合 {raw:?} ≠ {w:?}"));
            }
        }
        Check::Cursor(want) => {
            let cursor = engine.composition().map(|c| c.cursor);
            if cursor != Some(*want) {
                return Err(format!("光标 {cursor:?} ≠ {want:?}"));
            }
        }
        Check::NoComp => {
            if engine.composition().is_some() {
                return Err("组合应已清空".into());
            }
        }
        Check::ModeSeq(want) => {
            if modes != want.as_slice() {
                return Err(format!("模式 {modes:?} ≠ {want:?}"));
            }
        }
        Check::Passthrough => {
            if !passed {
                return Err("应有直通事件".into());
            }
        }
        Check::Hint { text, hint } => {
            let found = engine
                .candidates_detailed()
                .iter()
                .find(|c| c.text.as_str() == text.as_str());
            match found {
                Some(c) if c.hint.as_deref() == Some(hint.as_str()) => {}
                _ => {
                    return Err(format!(
                        "候选 {text:?} 注音 {:?} ≠ {hint:?}",
                        found.and_then(|c| c.hint.clone())
                    ));
                }
            }
        }
        Check::UserHas { key, word } => {
            let prefix = format!("{key}\t{word}\t");
            if !engine
                .export_user()
                .lines()
                .any(|line| line.starts_with(prefix.as_str()))
            {
                return Err(format!("用户词库缺少 {key} → {word}"));
            }
        }
        Check::GroupAtLeast { group, min } => {
            let count = engine
                .group_stats()
                .into_iter()
                .find_map(|(name, count)| (name == *group).then_some(count))
                .unwrap_or(0);
            if count < *min {
                return Err(format!("组 {group} 学习次数 {count} < {min}"));
            }
        }
    }
    Ok(())
}
