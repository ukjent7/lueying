//! 邮箱与网址 E2E:`用户名@` 出常见邮箱、`www.` 网址直通。
//!
//! 先列失败方式:
//! 1. @ 被当标点落定 → 邮箱模式进不去。
//! 2. 域名里的点被当句号 → 邮箱/网址断在 @ 或 www. 之后。
//! 3. 数字仍当候选序号 → 筛不出 `@163`。
//! 4. 地址候选写进用户词库或字级搭配 → 污染后续学习。
//! 5. 非 www 域名与拼音后的点被接管 → 「好。」打不出句号。
//! 6. 圆圈数字相关的准入变化吞掉正常标点。
//! 7. 表外域名没有原文候选 → 地址打不完、上不了屏。

use std::sync::{Arc, RwLock};

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Event, Key, SharedUser, UserDict};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

fn engine() -> Engine {
    Engine::new(Arc::new(Dictionary::builtin()), Config::default())
}

/// 按真实按键路径打一段地址:字母走 Letter、数字走 Digit、符号走 Punct。
fn typed(text: &str) -> Engine {
    let mut e = engine();
    for c in text.chars() {
        if let Some(d) = c.to_digit(10) {
            e.process(Key::Digit(d as u8));
        } else if c.is_ascii_lowercase() {
            e.process(Key::Letter(c));
        } else {
            e.process(Key::Punct(c));
        }
    }
    e
}

fn cands(text: &str) -> Vec<String> {
    typed(text).candidates()
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 用户名后 @ 进邮箱模式,常见域按常用度排前,原文兜底也在候选里。
    let list = cands("zhangsan@");
    push(
        &mut checks,
        "邮箱候选",
        list.first().map(String::as_str) == Some("zhangsan@qq.com")
            && list.contains(&"zhangsan@163.com".to_string())
            && list.contains(&"zhangsan@".to_string()),
        format!("{list:?}"),
    );
    // @ 后续字母筛域名;数字同样参与筛选。地址块之外词典可能还有
    // 字面候选,只对含 @ 的条目做约束。
    let list = cands("zhangsan@q");
    push(
        &mut checks,
        "字母筛域名",
        list.first().map(String::as_str) == Some("zhangsan@qq.com")
            && list
                .iter()
                .filter(|c| c.contains('@'))
                .all(|c| c.starts_with("zhangsan@q")),
        format!("{list:?}"),
    );
    let list = cands("zhangsan@1");
    push(
        &mut checks,
        "数字筛域名",
        list.contains(&"zhangsan@163.com".to_string())
            && list.contains(&"zhangsan@126.com".to_string())
            && !list.contains(&"zhangsan@qq.com".to_string()),
        format!("{list:?}"),
    );
    // 表外域名只剩原文候选在首位,仍能上屏。
    let list = cands("zhangsan@foobar");
    push(
        &mut checks,
        "表外域名剩原文",
        list.first().map(String::as_str) == Some("zhangsan@foobar"),
        format!("{list:?}"),
    );
    // 空格上屏完整地址,且不写用户词库、不学搭配。
    let user: SharedUser = Arc::new(RwLock::new(UserDict::new()));
    let mut e = Engine::new(Arc::new(Dictionary::builtin()), Config::default())
        .with_user(Arc::clone(&user));
    for c in "zhangsan@q".chars() {
        if let Some(d) = c.to_digit(10) {
            e.process(Key::Digit(d as u8));
        } else if c.is_ascii_lowercase() {
            e.process(Key::Letter(c));
        } else {
            e.process(Key::Punct(c));
        }
    }
    let events = e.process(Key::Space);
    let revision = user.read().unwrap_or_else(|p| p.into_inner()).revision();
    push(
        &mut checks,
        "邮箱上屏不学习",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "zhangsan@qq.com"))
            && revision == 0,
        format!("events {events:?} rev {revision}"),
    );

    // 网址:www 起头的点并入组合,域名、点与路径一直拼到上屏。
    let mut w = engine();
    for c in "www".chars() {
        w.process(Key::Letter(c));
    }
    let events = w.process(Key::Punct('.'));
    push(
        &mut checks,
        "www 起头并点",
        events.is_empty() && w.composition().map(|comp| comp.raw.as_str()) == Some("www."),
        format!(
            "events {events:?} raw {:?}",
            w.composition().map(|comp| comp.raw.clone())
        ),
    );
    let list = cands("www.example.com/train");
    push(
        &mut checks,
        "网址直通候选",
        list.contains(&"www.example.com/train".to_string()),
        format!("{list:?}"),
    );
    let events = typed("www.example.com").process(Key::Space);
    push(
        &mut checks,
        "网址上屏",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "www.example.com")),
        format!("{events:?}"),
    );

    // 负路径:普通拼音后的点仍是句号,非 www 域名也不接管。
    let mut h = engine();
    for c in "hao".chars() {
        h.process(Key::Letter(c));
    }
    let events = h.process(Key::Punct('.'));
    push(
        &mut checks,
        "拼音后点是句号",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "。")),
        format!("{events:?}"),
    );
    let events = typed("example").process(Key::Punct('.'));
    push(
        &mut checks,
        "非 www 域名不接管",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "。")),
        format!("{events:?}"),
    );
    // 空 raw 与非用户名字符后的 @ 按普通标点上屏。
    let events = engine().process(Key::Punct('@'));
    push(
        &mut checks,
        "空组合的 @ 直通",
        events
            .iter()
            .any(|event| matches!(event, Event::Commit(text) if text == "@")),
        format!("{events:?}"),
    );

    let failures = checks.iter().filter(|(_, ok, _)| !ok).count();
    report.push_str("## 邮箱与网址\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    for (name, ok, note) in &checks {
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if *ok { "PASS" } else { "FAIL" }
        ));
    }
    report.push('\n');
    failures
}
