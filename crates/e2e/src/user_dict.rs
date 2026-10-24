//! 用户词库 E2E:先列失败方式,再逐条检查。
//!
//! 1. 选词学习只留在当前会话 → 共用一份词库的两个引擎必须互相看到。
//! 2. 导入重复行重复计分 → 同键同词合并,频次取最大。
//! 3. 导出/导入丢词或坏行连坐 → 往返后词频不变,坏行单独跳过。
//! 4. 上屏不经过学习 → 空格上屏后导出里应出现该键与该词。
//! 5. 用户词压不过词典 → 高频导入词必须排到同键词典词前面。
//! 6. 分段上屏学错键 → 上屏前缀词时只学它吃掉的音节。

use std::sync::{Arc, RwLock};

use pinyin_dict::Dictionary;
use pinyin_engine::{Config, Engine, Event, Key, SharedUser, UserDict};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

fn engine(dict: &Arc<Dictionary>, user: &SharedUser) -> Engine {
    Engine::new(Arc::clone(dict), Config::default()).with_user(Arc::clone(user))
}

pub fn run(report: &mut String) -> usize {
    let dict = Arc::new(Dictionary::builtin());
    let mut checks = Vec::new();

    // 上屏路径:整键选词后导出应出现该键与该词。
    let mut e = Engine::new(Arc::clone(&dict), Config::default());
    for c in "nihao".chars() {
        e.process(Key::Letter(c));
    }
    e.process(Key::Space);
    let exported = e.export_user();
    push(
        &mut checks,
        "上屏写入用户词库",
        exported.lines().any(|l| l.starts_with("nihao\t你好\t")),
        exported.lines().next().unwrap_or_default().to_string(),
    );

    // 两个会话共用一份词库:一边导入,另一边立刻用它排序。
    let user = Arc::new(RwLock::new(UserDict::new()));
    let mut a = engine(&dict, &user);
    let mut b = engine(&dict, &user);
    a.import_user("shi\t十\t100000\t0\n");
    for c in "shi".chars() {
        b.process(Key::Letter(c));
    }
    let top = b.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "跨会话共享用户词库",
        top == "十",
        format!("首选 {top}"),
    );

    // 导入合并:同键同词只留一条,频次取最大。
    let mut c = Engine::new(Arc::clone(&dict), Config::default());
    c.import_user("shi\t十\t2\t0\n");
    c.import_user("shi\t十\t3\t0\n");
    let exported = c.export_user();
    let ten: Vec<_> = exported
        .lines()
        .filter(|l| l.starts_with("shi\t十\t"))
        .collect();
    push(
        &mut checks,
        "重复导入同键同词合并",
        ten.len() == 1 && ten[0].starts_with("shi\t十\t3\t"),
        format!("{} 条", ten.len()),
    );

    // 导出再导入(Server 落盘用的就是这份文本),词与频次都要还在。
    let text = a.export_user();
    let mut d = Engine::new(Arc::clone(&dict), Config::default());
    d.import_user(&text);
    for ch in "shi".chars() {
        d.process(Key::Letter(ch));
    }
    let top = d.candidates().first().cloned().unwrap_or_default();
    push(
        &mut checks,
        "导出导入往返",
        top == "十" && d.export_user().contains("shi\t十\t100000\t"),
        format!("首选 {top}"),
    );

    // 坏行、空键、坏频次都跳过,其余词条照常导入。
    let mut f = Engine::new(Arc::clone(&dict), Config::default());
    f.import_user("坏行\nshi\t十\tx\t0\n\tshi\t十\t1\t0\nshi\t十\t4\t0\n");
    let lines: Vec<_> = f.export_user().lines().collect();
    push(
        &mut checks,
        "坏行跳过",
        lines.len() == 1 && lines[0].starts_with("shi\t十\t4\t"),
        format!("{} 条", lines.len()),
    );

    // 分段上屏:选「你」只吃 ni,学习键也应是 ni。
    let mut g = Engine::new(Arc::clone(&dict), Config::default());
    for ch in "nihao".chars() {
        g.process(Key::Letter(ch));
    }
    g.process(Key::Down);
    let committed: String = g
        .process(Key::Space)
        .into_iter()
        .filter_map(|event| match event {
            Event::Commit(text) => Some(text),
            _ => None,
        })
        .collect();
    let exported = g.export_user();
    push(
        &mut checks,
        "分段上屏只学吃掉的音节",
        committed == "你" && exported.contains("ni\t你\t") && !exported.contains("nihao\t你\t"),
        format!("上屏 {committed:?}"),
    );

    report.push_str("\n## 用户词库\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
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
