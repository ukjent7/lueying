//! 本地打字记录:开关、干净句与编辑句的落盘行、私密范围不记、文件追加与清空。

use std::sync::Arc;

use crate::{Config, Engine, Key};
use lueying_dict::Dictionary;

use super::{Sentence, append, clear};

fn engine() -> Engine {
    let (dict, bad) = Dictionary::from_text("微笑\t1000000\twei'xiao\n");
    assert_eq!(bad, 0);
    Engine::new(Arc::new(dict), Config::default())
}

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

#[test]
fn off_by_default_records_nothing() {
    let mut engine = engine();
    type_keys(&mut engine, "weixiao .");
    assert!(engine.take_typing_log().is_empty());
}

#[test]
fn clean_sentence_is_an_eval_row_ended_by_punctuation() {
    let mut engine = engine();
    engine.set_typing_log(true);
    type_keys(&mut engine, "weixiao.");
    assert_eq!(
        engine.take_typing_log(),
        vec![Sentence {
            clean: true,
            line: "user-live\tuser\tweixiao\t微笑\t".to_string(),
        }]
    );
}

#[test]
fn deleted_committed_text_goes_to_edits_with_full_trail() {
    let mut engine = engine();
    engine.set_typing_log(true);
    // 上屏「微笑」后退格删掉「笑」,回车落定:编辑句,键入串仍是原样击键。
    type_keys(&mut engine, "weixiao \u{8}\n");
    assert_eq!(
        engine.take_typing_log(),
        vec![Sentence {
            clean: false,
            line: "edit-live\tuser\tweixiao\t微\t\tweixiao_#@".to_string(),
        }]
    );
}

#[test]
fn composing_backspace_removes_the_letter_from_the_input() {
    let mut engine = engine();
    engine.set_typing_log(true);
    type_keys(&mut engine, "weixiaoo\u{8}.");
    let rows = engine.take_typing_log();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].clean, "组合内退格不算编辑已上屏文字");
    assert!(rows[0].line.starts_with("user-live\tuser\tweixiao\t微笑\t"));
}

#[test]
fn private_scope_records_nothing_and_drops_the_open_sentence() {
    let mut engine = engine();
    engine.set_typing_log(true);
    type_keys(&mut engine, "wei");
    engine.set_learning(false);
    type_keys(&mut engine, "xiao.");
    assert!(engine.take_typing_log().is_empty());
}

#[test]
fn focus_loss_settles_the_sentence() {
    let mut engine = engine();
    engine.set_typing_log(true);
    type_keys(&mut engine, "weixiao");
    engine.flush();
    assert_eq!(
        engine.take_typing_log(),
        vec![Sentence {
            clean: true,
            line: "user-live\tuser\tweixiao\t微笑\t".to_string(),
        }]
    );
}

#[test]
fn append_and_clear_only_touch_the_record_files() {
    let dir = std::env::temp_dir().join(format!("lueying-typing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    append(&dir, &[]).expect("空句子不写");
    assert!(!dir.exists(), "没有句子就不建目录");
    let sentences = [Sentence {
        clean: true,
        line: "user-live\tuser\tweixiao\t微笑\t".to_string(),
    }];
    append(&dir, &sentences).expect("追加");
    append(&dir, &sentences).expect("再追加");
    let text = std::fs::read_to_string(dir.join("typing-log.tsv")).expect("记录文件");
    assert_eq!(text.matches("user-live\tuser\tweixiao\t微笑").count(), 2);
    assert_eq!(text.matches("# 本地打字记录").count(), 1, "表头只写一次");
    clear(&dir).expect("清空");
    assert!(!dir.join("typing-log.tsv").exists());
    clear(&dir).expect("重复清空不报错");
    let _ = std::fs::remove_dir_all(&dir);
}
