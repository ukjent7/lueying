//! 功能模块 E2E:注册一个最小模块,验证候选注入、上屏转换与默认路径隔离。
//!
//! 失败方式:
//! 1. 模块注册后不执行 → 候选列表里看不到模块注入项。
//! 2. 模块改了候选,但上屏仍用旧文本 → 提交结果不包含转换后的写法。
//! 3. 默认注册表被意外启用 → 不注册模块的引擎也会出现扩展候选/转换。

use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_engine::module::{
    CandidateContext, Capability, CommitContext, Manifest, Module, Modules,
};
use lueying_engine::{Candidate, Config, Engine, Event, Key};

struct Demo;

impl Module for Demo {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "e2e-demo",
            name: "E2E 演示模块",
            version: env!("CARGO_PKG_VERSION"),
            capabilities: &[Capability::Candidates, Capability::Commit],
            triggers: &[],
        }
    }

    fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        if ctx.raw == "hao" {
            let at = 1.min(cands.len());
            cands.insert(at, Candidate::whole(ctx.raw.len(), "扩展", 1.0));
        }
    }

    fn commit(&self, _ctx: &CommitContext<'_>, text: &mut String) {
        if text.as_str() == "好" {
            text.push('！');
        }
    }
}

fn engine(modules: Modules) -> Engine {
    let script = Arc::new(lueying_features::ScriptModule::new(
        lueying_features::Script::Simplified,
    ));
    Engine::new(Arc::new(Dictionary::builtin()), Config::default()).with_modules(Arc::new(
        lueying_features::complete_builtins(modules, &script),
    ))
}

fn type_hao(engine: &mut Engine) {
    for c in "hao".chars() {
        engine.process(Key::Letter(c));
    }
}

pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    // 注册模块:候选注入与上屏转换都应生效。
    let mut extended = engine(Modules::new().register(Arc::new(Demo)));
    type_hao(&mut extended);
    let candidates = extended.candidates();
    let injected = candidates
        .get(1)
        .is_some_and(|text| text.as_str() == "扩展");
    checks.push((
        "模块候选注入",
        injected,
        format!("第 2 条 = {:?}", candidates.get(1)),
    ));
    let commits = extended.process(Key::Space);
    let transformed = commits
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text.as_str() == "好！"));
    checks.push(("模块上屏转换", transformed, format!("{commits:?}")));

    // 空注册表:核心行为与未接入模块时一致。
    let mut plain = engine(Modules::new());
    type_hao(&mut plain);
    let plain_commits = plain.process(Key::Space);
    let untouched = plain_commits.len() == 1
        && matches!(&plain_commits[0], Event::Commit(text) if text.as_str() == "好");
    checks.push(("默认路径不受影响", untouched, format!("{plain_commits:?}")));

    report.push_str("\n## 功能模块扩展点\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}
