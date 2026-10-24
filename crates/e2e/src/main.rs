//! E2E 运行器:跑场景规格与主词典探针,产出可复查的 Markdown 报告并按结果设退出码。
//! 用法:`pinyin-e2e [--dict 主词典.tsv]`

mod scenarios;

use std::path::PathBuf;
use std::time::Instant;

use pinyin_dict::Dictionary;
use pinyin_engine::{Config, Engine, Event, Key, Mode};
use scenarios::{scenarios, probes, Check, Scenario};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dict_path = args
        .iter()
        .position(|a| a == "--dict")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

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

    // 主词典探针与性能
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
        let mut engine = Engine::new(dict, Config::default());
        let mut samples: Vec<f64> = Vec::new();
        for (typed, want) in probes() {
            engine.process(Key::Esc);
            for c in typed.chars() {
                let t = Instant::now();
                engine.process(Key::Letter(c));
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let top = engine.candidates().first().cloned().unwrap_or_default();
            if top == want {
                report.push_str(&format!("| {typed} | {want} | {top} | PASS |\n"));
            } else {
                failures += 1;
                report.push_str(&format!("| {typed} | {want} | {top} | FAIL |\n"));
            }
            engine.process(Key::Esc);
        }
        let count = samples.len();
        let avg = samples.iter().sum::<f64>() / count.max(1) as f64;
        let max = samples.iter().cloned().fold(0.0f64, f64::max);
        report.push_str(&format!(
            "\n## 性能(主词典,{count} 键)\n\n- 平均 {avg:.2} ms(阈值 5)\n- 最大 {max:.2} ms(阈值 10)\n"
        ));
        if avg > 5.0 || max > 10.0 {
            failures += 1;
            report.push_str("\n**性能超阈值**\n");
        }
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

fn run_scenario(sc: &Scenario) -> Result<(), String> {
    let mut cfg = Config::default();
    if sc.fuzzy {
        cfg.fuzzy.zcs = true;
    }
    let mut engine = Engine::new(Dictionary::builtin(), cfg);
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
            if commits != want {
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
        Check::NoComp => {
            if engine.composition().is_some() {
                return Err("组合应已清空".into());
            }
        }
        Check::ModeSeq(want) => {
            if *modes != *want {
                return Err(format!("模式 {modes:?} ≠ {want:?}"));
            }
        }
        Check::Passthrough => {
            if !passed {
                return Err("应有直通事件".into());
            }
        }
    }
    Ok(())
}
