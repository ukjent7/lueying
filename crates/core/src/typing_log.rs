//! 本地打字记录:评测语料的来源。默认关闭,只写本机,从不上传。
//!
//! 一句从句首开始,到句号、问号、感叹号、回车、失焦落定为止。组合未落定时不收句,
//! 超过长度上限的句子直接丢弃。句子分两类落盘:
//!
//! - 干净句(只有上屏,没有删除已上屏文字、没有英文直通)进 `typing-log.tsv`,
//!   列与 `dicts/eval-user-*.tsv` 相同,可直接 `lueying-bench --corpus` 使用;
//! - 编辑过的句子进 `typing-edits.tsv`,键入串是原样击键(含被删掉的字母),
//!   末列 `trail` 是完整按键流:字母与数字原样,`_` 空格,`@` 回车,`<` 组合内退格,
//!   `#` 删除已上屏文字,`^` Shift,其余键记 `~`。
//!
//! 私密输入范围与关闭学习的应用同样不记(沿用学习开关这一私密标记),
//! 这种时候未落定的句子也一并丢弃。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use crate::Engine;
use crate::types::{Event, Key};

/// 句子长度上限(击键串字节数):超过就丢弃,免得没有标点的长段一直积累。
const MAX_TRAIL: usize = 200;

const CLEAN_FILE: &str = "typing-log.tsv";
const EDIT_FILE: &str = "typing-edits.tsv";
const CLEAN_HEADER: &str = "# 本地打字记录(仅评测,不上传)。列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context(空)\n";
const EDIT_HEADER: &str = "# 编辑过的句子(仅评测,不上传)。pinyin 为原样击键;列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context(空)<TAB>trail\n";

/// 收齐的一句。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sentence {
    /// 干净句写 `typing-log.tsv`,编辑过的写 `typing-edits.tsv`。
    pub clean: bool,
    /// 一行 TSV,不含换行。
    pub line: String,
}

/// 单个会话的句子缓冲,只在 [`Engine`] 里用。
#[derive(Debug)]
pub(crate) struct Recorder {
    on: bool,
    /// 当前句的击键流(含编辑键)。
    trail: String,
    /// 当前句上屏的文字(删除时同步回退)。
    text: String,
    /// 没有删除已上屏文字、没有字面或英文直通。
    clean: bool,
    done: Vec<Sentence>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            on: false,
            trail: String::new(),
            text: String::new(),
            clean: true,
            done: Vec::new(),
        }
    }
}

impl Recorder {
    /// 记一次按键(`key` 为 None 表示只落定,例如失焦)与它产生的事件。
    /// `composing` 是按键前组合是否非空,`pending` 是按键后组合是否仍非空。
    fn note(&mut self, key: Option<Key>, composing: bool, pending: bool, events: &[Event]) {
        let mut end = key.is_none();
        if let Some(key) = key {
            self.trail.push(token(key, composing));
            end = key == Key::Enter;
        }
        for event in events {
            match event {
                Event::Commit(text) => {
                    // 字面文本(没有候选的拼音原文、英文)不是按拼音打出来的字。
                    if text.chars().any(|c| c.is_ascii_alphanumeric()) {
                        self.clean = false;
                    }
                    self.text.push_str(text);
                    end |= text
                        .chars()
                        .any(|c| matches!(c, '。' | '！' | '？' | '!' | '?'));
                }
                Event::Passthrough(Key::Space) => {}
                Event::Passthrough(Key::Enter) => end = true,
                Event::Passthrough(Key::Backspace | Key::SegmentBackspace) => {
                    // 删除已上屏的文字:句子被编辑过。
                    self.clean = false;
                    self.text.pop();
                }
                Event::Passthrough(_) | Event::Action(_) => self.clean = false,
                Event::Mode(_) => {}
            }
        }
        if self.trail.len() >= MAX_TRAIL {
            self.discard();
        } else if end && !pending {
            self.close();
        }
    }

    /// 句子收齐:组出一行并排队。没有拼音或文字的句子不记。
    fn close(&mut self) {
        let pinyin = letters(&self.trail);
        let expected: String = self.text.chars().filter(|c| c.is_alphanumeric()).collect();
        if !pinyin.is_empty() && !expected.is_empty() {
            let line = if self.clean {
                format!("user-live\tuser\t{pinyin}\t{expected}\t")
            } else {
                format!("edit-live\tuser\t{pinyin}\t{expected}\t\t{}", self.trail)
            };
            self.done.push(Sentence {
                clean: self.clean,
                line,
            });
        }
        self.discard();
    }

    /// 丢掉未落定的句子。
    fn discard(&mut self) {
        self.trail.clear();
        self.text.clear();
        self.clean = true;
    }
}

/// 击键对应的记录字符。`composing` 区分组合内退格(删拼音)与上屏后的退格(删文字)。
fn token(key: Key, composing: bool) -> char {
    match key {
        Key::Letter(c) => c,
        Key::Digit(d) => char::from(b'0' + d.min(9)),
        Key::Space => '_',
        Key::Enter => '@',
        Key::Backspace | Key::SegmentBackspace if composing => '<',
        Key::Backspace | Key::SegmentBackspace => '#',
        Key::Shift => '^',
        Key::Punct(p) if !"<#@_^~".contains(p) => p,
        _ => '~',
    }
}

/// 击键流里留下的小写字母:组合内退格删掉前一个字母。
fn letters(trail: &str) -> String {
    let mut out = String::new();
    for c in trail.chars() {
        match c {
            '<' => {
                out.pop();
            }
            'a'..='z' => out.push(c),
            _ => {}
        }
    }
    out
}

impl Engine {
    /// 打开或关闭本地打字记录(设置里的开关)。关闭时丢掉未落定的句子。
    pub fn set_typing_log(&mut self, on: bool) {
        if !on {
            self.typing.discard();
        }
        self.typing.on = on;
    }

    /// 取走已收齐的句子,由调用方落盘。
    pub fn take_typing_log(&mut self) -> Vec<Sentence> {
        std::mem::take(&mut self.typing.done)
    }

    /// 把一次按键或落定交给记录器;私密输入范围里整句丢弃。
    pub(crate) fn note_typing(&mut self, key: Option<Key>, composing: bool, events: &[Event]) {
        if !self.typing.on {
            return;
        }
        if !self.learning {
            self.typing.discard();
            return;
        }
        let pending = self.comp.is_some();
        self.typing.note(key, composing, pending, events);
    }
}

/// 把句子追加到记录目录:干净句进 `typing-log.tsv`,编辑过的进 `typing-edits.tsv`。
/// 没有对应的句子就不建文件,也不建目录。
pub fn append(dir: &Path, sentences: &[Sentence]) -> std::io::Result<()> {
    for (clean, name, header) in [
        (true, CLEAN_FILE, CLEAN_HEADER),
        (false, EDIT_FILE, EDIT_HEADER),
    ] {
        let lines: Vec<&str> = sentences
            .iter()
            .filter(|sentence| sentence.clean == clean)
            .map(|sentence| sentence.line.as_str())
            .collect();
        if lines.is_empty() {
            continue;
        }
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        let mut text = String::new();
        if !path.exists() {
            text.push_str(header);
        }
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(text.as_bytes())?;
    }
    Ok(())
}

/// 清空记录:删掉两份记录文件,缺失的不算错。
pub fn clear(dir: &Path) -> std::io::Result<()> {
    for name in [CLEAN_FILE, EDIT_FILE] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "typing_log_tests.rs"]
mod tests;
