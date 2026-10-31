//! 动作候选 E2E:触发词整串匹配、空格照旧上屏文字、数字/点选执行命令、
//! 字面负反馈降频与置顶。
//!
//! 失败方式:
//! 1. 动作候选抢占首位 → 空格上屏命令标题或执行命令,文字首选丢失。
//! 2. 只做前缀匹配 → 半截拼音就冒出动作候选。
//! 3. 字面原文候选被英文/快捷路径跳过 → 用户没有要字母的退路。
//! 4. 负反馈没记到触发词上 → 用户反复选字面后动作候选仍出现。

use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_engine::{Action, Config, Engine, Event, Key};

fn engine() -> Engine {
    Engine::new(Arc::new(Dictionary::builtin()), Config::default())
        .with_modules(crate::builtins())
}

fn action(trigger: &str, title: &str) -> Action {
    Action {
        trigger: trigger.to_string(),
        id: "demo.echo".to_string(),
        title: title.to_string(),
    }
}

fn type_text(engine: &mut Engine, text: &str) {
    for c in text.chars() {
        engine.process(Key::Letter(c));
    }
}

fn commit_text(events: Vec<Event>) -> Option<String> {
    events.into_iter().find_map(|event| match event {
        Event::Commit(text) => Some(text),
        _ => None,
    })
}

fn check(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

/// 点选页尾的字面原文候选;动作候选出现时总会补一条。
fn pick_literal(engine: &mut Engine) -> Option<String> {
    let index = engine.candidates().iter().position(|text| text == "bc")?;
    commit_text(engine.pick(index))
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 有中文候选的触发词:动作候选排第二,字面原文补在页尾,空格上屏首选文字。
    let mut e = engine();
    e.set_action(Some(action("nihao", "打个招呼")));
    type_text(&mut e, "nihao");
    let cands = e.candidates();
    let top = cands.first().cloned().unwrap_or_default();
    let acted_at = cands.iter().position(|text| text == "打个招呼");
    let literal = cands.iter().any(|text| text == "nihao");
    let committed = commit_text(e.process(Key::Space));
    check(
        &mut checks,
        "空格上屏首选文字",
        !top.is_empty()
            && top != "打个招呼"
            && acted_at == Some(1)
            && literal
            && committed.as_deref() == Some(top.as_str())
            && e.composition().is_none(),
        format!("候选 {cands:?} 空格 → {committed:?}"),
    );

    // Enter 照旧上屏原文,动作候选不拦截。
    let mut e = engine();
    e.set_action(Some(action("nihao", "打个招呼")));
    type_text(&mut e, "nihao");
    let before = e.candidates();
    let committed = commit_text(e.process(Key::Enter));
    check(
        &mut checks,
        "Enter 上屏原文",
        committed.as_deref() == Some("nihao") && e.composition().is_none(),
        format!("候选 {before:?} 回车 → {committed:?}"),
    );

    // 数字键选中动作候选:执行命令,不上屏文字,组合清空。
    let mut e = engine();
    e.set_action(Some(action("nihao", "打个招呼")));
    type_text(&mut e, "nihao");
    let events = e.process(Key::Digit(2));
    let acted = events
        .iter()
        .any(|event| matches!(event, Event::Action(id) if id == "demo.echo"));
    let committed = events.iter().any(|event| matches!(event, Event::Commit(_)));
    check(
        &mut checks,
        "数字选择执行命令",
        acted && !committed && e.composition().is_none() && e.candidates().is_empty(),
        format!("{events:?}"),
    );

    // 候选窗点选动作候选走同一条执行路径。
    let mut e = engine();
    e.set_action(Some(action("nihao", "打个招呼")));
    type_text(&mut e, "nihao");
    let index = e
        .candidates()
        .iter()
        .position(|text| text == "打个招呼")
        .unwrap_or(0);
    let events = e.pick(index);
    check(
        &mut checks,
        "点选执行命令",
        events
            .iter()
            .any(|event| matches!(event, Event::Action(id) if id == "demo.echo"))
            && e.composition().is_none(),
        format!("下标 {index} → {events:?}"),
    );

    // 只做整串完全匹配:少一个字母不出动作候选。
    let mut e = engine();
    e.set_action(Some(action("nihao", "打个招呼")));
    type_text(&mut e, "niha");
    check(
        &mut checks,
        "只整串匹配",
        !e.candidates().iter().any(|text| text == "打个招呼"),
        format!("niha → {:?}", e.candidates()),
    );

    // 负反馈:连续两次点选字面原文后,该触发词的动作候选不再出现。
    // `bc` 在词典里能出「班」,所以字面原文只能靠点选/数字选中,空格仍是中文首选。
    let mut e = engine();
    let act = action("bc", "运行演示");
    e.set_action(Some(act.clone()));
    type_text(&mut e, "bc");
    let showed = e.candidates().iter().any(|text| text == "运行演示");
    let first = pick_literal(&mut e);
    e.set_action(Some(act.clone()));
    type_text(&mut e, "bc");
    let second = pick_literal(&mut e);
    e.set_action(Some(act.clone()));
    type_text(&mut e, "bc");
    let after = e.candidates();
    let lingers = after.iter().any(|text| text == "运行演示");
    // 动作候选被抑制后不再补字面候选;Enter 仍照旧上屏原文。
    let raw = commit_text(e.process(Key::Enter));
    check(
        &mut checks,
        "字面负反馈降频",
        showed
            && first.as_deref() == Some("bc")
            && second.as_deref() == Some("bc")
            && !lingers
            && raw.as_deref() == Some("bc"),
        format!("两次选原文 {first:?}/{second:?} 之后 {after:?},Enter {raw:?}"),
    );

    // 置顶:动作候选排到首位,数字 1 执行命令,字面原文仍在候选里。
    let mut e = engine();
    e.set_action(Some(action("bc", "运行演示")));
    type_text(&mut e, "bc");
    let index = e
        .candidates()
        .iter()
        .position(|text| text == "运行演示")
        .unwrap_or(0);
    let pinned = e.pin(index);
    let top = e.candidates().first().cloned().unwrap_or_default();
    let literal = e.candidates().iter().any(|text| text == "bc");
    let events = e.process(Key::Digit(1));
    check(
        &mut checks,
        "置顶动作候选",
        pinned
            && top == "运行演示"
            && literal
            && events
                .iter()
                .any(|event| matches!(event, Event::Action(id) if id == "demo.echo"))
            && e.composition().is_none(),
        format!("置顶 {pinned} 首位 {top:?} → {events:?}"),
    );

    report.push_str("\n## 动作候选:触发词 / 空格 / 数字 / 负反馈 / 置顶\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0usize;
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
