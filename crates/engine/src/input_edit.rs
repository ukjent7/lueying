//! 组合编辑:写入字符、回删、按音节移动与删除、清空组合。
//!
//! 与 `input`、`state` 等分文件只是职责边界:`Engine` 的 inherent impl,
//! 共享 crate 根里的私有字段。跨文件调用的方法显式标为 `pub(crate)`。

use super::*;

impl Engine {
    /// 把快捷/表达式字符写进组合;组合已满时返回 `false`,由调用方落定。
    pub(crate) fn compose_char(&mut self, c: char) -> bool {
        let inserted = {
            let Some(comp) = self.comp.as_mut() else {
                return false;
            };
            if comp.raw.len() >= MAX_RAW {
                false
            } else {
                comp.raw.insert(comp.cursor, c);
                comp.cursor += 1;
                true
            }
        };
        if inserted {
            self.recompute();
        }
        inserted
    }

    pub(crate) fn try_insert_apostrophe(&mut self) {
        let ok = if let Some(comp) = self.comp.as_ref() {
            comp.seg_spans.iter().rev().any(|&(s, e)| {
                e == comp.cursor && lueying_dict::syllables::is_syllable(&comp.raw[s..e])
            })
        } else {
            false
        };
        if ok
            && let Some(comp) = self.comp.as_mut()
            && comp.raw.len() < MAX_RAW
        {
            comp.raw.insert(comp.cursor, '\'');
            comp.cursor += 1;
            self.recompute();
        }
    }

    /// 逐字母回删/前删;与微软拼音、微信输入法一致,不按音节整段删。
    pub(crate) fn delete_char(&mut self, left: bool) {
        let Some(comp) = self.comp.as_mut() else {
            return;
        };
        if left {
            if comp.cursor == 0 {
                return;
            }
            let at = comp.cursor - 1;
            comp.raw.remove(at);
            comp.cursor = at;
        } else {
            if comp.cursor >= comp.raw.len() {
                return;
            }
            comp.raw.remove(comp.cursor);
        }
        if comp.raw.is_empty() {
            self.comp = None;
            self.comp_committed = false;
            self.phrase = None;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            self.recompute();
        }
    }

    /// 光标左右移动一个字符。
    pub(crate) fn move_cursor_char(&mut self, dir: i32) {
        let Some(comp) = self.comp.as_mut() else {
            return;
        };
        comp.cursor = if dir < 0 {
            comp.cursor.saturating_sub(1)
        } else {
            (comp.cursor + 1).min(comp.raw.len())
        };
    }

    /// Ctrl+Left/Ctrl+Right:按音节移动光标。音节边界取音节图的最长匹配路径,
    /// 与候选无关;隔音符只当分隔,不占音节。
    pub(crate) fn move_segment(&mut self, dir: i32) {
        let Some(comp) = self.comp.as_ref() else {
            return;
        };
        let spans = self.scheme.segments(&comp.raw);
        let bytes = comp.raw.as_bytes();
        let cursor = comp.cursor;
        let next = if dir < 0 {
            let mut at = cursor;
            while at > 0 && bytes[at - 1] == b'\'' {
                at -= 1;
            }
            spans
                .iter()
                .rev()
                .find(|span| span.0 < at && at <= span.1)
                .map_or(0, |span| span.0)
        } else {
            let mut at = cursor;
            while at < bytes.len() && bytes[at] == b'\'' {
                at += 1;
            }
            spans
                .iter()
                .find(|span| span.0 <= at && at < span.1)
                .map_or(bytes.len(), |span| span.1)
        };
        if let Some(comp) = self.comp.as_mut() {
            comp.cursor = next;
        }
    }

    /// Ctrl+Backspace/Ctrl+Delete:删除光标前/后一个音节,连同相邻的一个隔音符。
    pub(crate) fn delete_segment(&mut self, left: bool) {
        let Some(comp) = self.comp.as_ref() else {
            return;
        };
        let mut raw = comp.raw.clone();
        let cursor = comp.cursor;
        let spans = self.scheme.segments(&raw);
        let bytes = raw.as_bytes();
        let (mut start, mut end) = if left {
            let mut at = cursor;
            while at > 0 && bytes[at - 1] == b'\'' {
                at -= 1;
            }
            let Some(span) = spans.iter().rev().find(|span| span.0 < at && at <= span.1) else {
                return;
            };
            (span.0, cursor)
        } else {
            let mut at = cursor;
            while at < bytes.len() && bytes[at] == b'\'' {
                at += 1;
            }
            let Some(span) = spans.iter().find(|span| span.0 <= at && at < span.1) else {
                return;
            };
            (cursor, span.1)
        };
        // 删掉音节时连同它一侧的隔音符一起收走,避免留下孤立的 `'`。
        if start > 0 && bytes[start - 1] == b'\'' {
            start -= 1;
        } else if end < bytes.len() && bytes[end] == b'\'' {
            end += 1;
        }
        if start >= end {
            return;
        }
        raw.replace_range(start..end, "");
        if raw.is_empty() {
            self.clear_comp();
            return;
        }
        if let Some(comp) = self.comp.as_mut() {
            comp.raw = raw;
            comp.cursor = start.min(comp.raw.len());
        }
        self.recompute();
    }

    pub(crate) fn clear_comp(&mut self) {
        self.comp = None;
        self.comp_committed = false;
        self.phrase = None;
        self.cands.clear();
        self.hl = 0;
        self.page = 0;
        self.rev = self.rev.wrapping_add(1);
    }
}
