//! 本地打字记录 E2E:默认关闭不记;开启后一句落成评测行;编辑过的句子进编辑记录;
//! 私密输入范围不记;没有句子时不建文件。

use std::sync::Arc;

use lueying_core::typing_log;
use lueying_core::{Config, Engine, Key};
use lueying_dict::Dictionary;

use crate::harness::{Checks, Temp, push};

fn engine(dict: &Arc<Dictionary>) -> Engine {
    Engine::new(Arc::clone(dict), Config::default())
}

/// 按字符模拟击键:空格、回车、退格与句号映射到对应的键。
fn type_keys(engine: &mut Engine, keys: &str) {
    for c in keys.chars() {
        let key = match c {
            ' ' => Key::Space,
            '\n' => Key::Enter,
            '\u{8}' => Key::Backspace,
            '.' => Key::Punct('.'),
            c => Key::Letter(c),
        };
        engine.process(key);
    }
}

pub fn run(report: &mut String) -> usize {
    let (dict, _) = Dictionary::from_text("微笑\t1000000\twei'xiao\n");
    let dict = Arc::new(dict);
    let temp = Temp::new("typing");
    let mut checks = Checks::new();

    // 默认关闭:打一句不记录,也不建文件。
    let mut off = engine(&dict);
    type_keys(&mut off, "weixiao.");
    let off_rows = off.take_typing_log();
    let off_dir = temp.root.join("off");
    let _ = typing_log::append(&off_dir, &off_rows);
    push(
        &mut checks,
        "关闭时不记录也不建文件",
        off_rows.is_empty() && !off_dir.exists(),
        format!("句子 {} 句", off_rows.len()),
    );

    // 开启:干净句落成评测行,表头只在首次写入。
    let on_dir = temp.root.join("on");
    let mut on = engine(&dict);
    on.set_typing_log(true);
    type_keys(&mut on, "weixiao.");
    let rows = on.take_typing_log();
    let _ = typing_log::append(&on_dir, &rows);
    let clean = std::fs::read_to_string(on_dir.join("typing-log.tsv")).unwrap_or_default();
    let line = "user-live\tuser\tweixiao\t微笑\t";
    push(
        &mut checks,
        "开启后一句落成评测行",
        rows.len() == 1 && clean.lines().any(|row| row == line),
        format!("{} 句,文件 {} 字节", rows.len(), clean.len()),
    );

    // 编辑过的句子(上屏后退格、回车落定)进编辑记录,不进评测行。
    type_keys(&mut on, "weixiao \u{8}\n");
    let edits = on.take_typing_log();
    let _ = typing_log::append(&on_dir, &edits);
    let edit_text = std::fs::read_to_string(on_dir.join("typing-edits.tsv")).unwrap_or_default();
    let edit_line = "edit-live\tuser\tweixiao\t微\t\tweixiao_#@";
    let clean_after = std::fs::read_to_string(on_dir.join("typing-log.tsv")).unwrap_or_default();
    push(
        &mut checks,
        "编辑过的句子进编辑记录",
        edits.len() == 1
            && edit_text.lines().any(|row| row == edit_line)
            && clean_after.lines().all(|row| !row.starts_with("edit-live")),
        format!("{} 句", edits.len()),
    );

    // 私密输入范围不记,未落定的句子一并丢弃。
    let mut private = engine(&dict);
    private.set_typing_log(true);
    type_keys(&mut private, "wei");
    private.set_learning(false);
    type_keys(&mut private, "xiao.");
    let private_rows = private.take_typing_log();
    push(
        &mut checks,
        "私密字段不记",
        private_rows.is_empty(),
        format!("句子 {} 句", private_rows.len()),
    );

    // 清空只删记录文件。
    let _ = typing_log::clear(&on_dir);
    push(
        &mut checks,
        "清空只删记录文件",
        !on_dir.join("typing-log.tsv").exists() && !on_dir.join("typing-edits.tsv").exists(),
        String::new(),
    );

    crate::harness::write(report, "本地打字记录", checks)
}
