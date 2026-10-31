//! 上屏与学习:落定候选、吃音节、词序、自动造词与负反馈。
//!
//! 与 `input`、`state` 等分文件只是职责边界:`Engine` 的 inherent impl,
//! 共享 crate 根里的私有字段。跨文件调用的方法显式标为 `pub(crate)`。

use super::*;

impl Engine {
    /// 逐段落定整个组合;没有候选时上屏原文。返回按序的上屏事件。
    pub(crate) fn commit_all(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(text) = self.commit_highlighted() {
            events.push(Event::Commit(text));
        }
        events
    }

    /// 落定整个组合后上屏一段字面文本;已选片段不再拼进整句词条。
    pub(crate) fn commit_all_then(&mut self, text: String) -> Vec<Event> {
        let mut events = self.commit_all();
        self.phrase = None;
        events.push(Event::Commit(text));
        events
    }

    /// 上屏高亮候选;无候选时上屏原文。动作候选是命令入口:标点、切模式与
    /// 失焦落定都不执行它,退到第一条文字候选;执行只走空格/数字/点选。
    pub(crate) fn commit_highlighted(&mut self) -> Option<String> {
        self.comp.as_ref()?;
        if self.cands.is_empty() {
            let raw = self.comp.take().map(|c| c.raw);
            self.clear_comp();
            return raw;
        }
        let mut idx = self.hl.min(self.cands.len() - 1);
        if self.cands[idx].action.is_some() {
            idx = self
                .cands
                .iter()
                .position(|cand| cand.action.is_none())
                .unwrap_or(idx);
        }
        self.apply_consume(idx)
    }

    /// 选中动作候选:清空组合并返回完整命令 id。命令不上屏文字,组合直接结束。
    pub(crate) fn take_action(&mut self, idx: usize) -> Option<String> {
        let id = self.cands.get(idx)?.action.clone()?;
        self.clear_comp();
        Some(id)
    }

    /// 应用第 idx 个候选:上屏文本,吃掉 consume 个音节,剩余留作组合并学习。
    pub(crate) fn apply_consume(&mut self, idx: usize) -> Option<String> {
        let cand = self.cands.get(idx)?.clone();
        let cut = cand.spans.get(cand.consume - 1).map(|s| s.1)?;
        let raw = self.comp.as_ref()?.raw.clone();
        // 用户明确选了字面原文而没有选动作候选:对触发词记一次同键负分;
        // 选够多次后不再注入动作候选,说明用户要的就是这段字母。
        if self.learning
            && cand.action.is_none()
            && cand.text == raw
            && let Some(action) = self.action.as_ref()
        {
            let key = raw.replace('\'', "");
            if key == action.trigger {
                let id = format!("!{}", action.id);
                self.user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .reject(&key, &id);
            }
        }
        // 学习与上下文用规范字形:繁体显示时仍写简体,切回简体不召回繁体词。
        let learn_text = cand
            .learn_text
            .as_deref()
            .unwrap_or(cand.text.as_str())
            .to_string();
        let fixed = cand.quick || cand.no_learn;
        // 快捷/固定候选不是词:不写用户词库、不学组级适应,也不把日期/金额
        // 当作用词接进词级转移。它们也不参与同一次组合的自动造词。
        if cand.quick {
            self.quick_handled += 1;
        }
        if fixed {
            self.phrase = None;
        }
        // 用户词库的键必须和查询时的音节键一致(ni'hao),不能存原始串(nihao),
        // 否则多音节词学完也匹配不上。
        let learned = if fixed {
            String::new()
        } else {
            cand.learn_key.clone().unwrap_or_else(|| {
                self.scheme
                    .key_for_spans(&raw, &cand.spans[..cand.consume.min(cand.spans.len())])
            })
        };
        if self.learning && !fixed {
            let mut user = self.user.write().unwrap_or_else(|p| p.into_inner());
            // 同键下被越过的候选记一次小负分。只统计吃掉同样多音节的候选:
            // 用户改选前缀词上屏时,不能被当成否定了整句读法。手误纠正的候选
            // 是推测出来的键,不拿它给正确输入留下负分。
            for skipped in self.cands.iter().take(idx) {
                if skipped.quick || skipped.no_learn {
                    continue;
                }
                if skipped.consume != cand.consume {
                    continue;
                }
                let scheme_key = self.scheme.key_for_spans(&raw, &skipped.spans);
                // 纠正/补全候选的学习键是推测出来的,和原始击键的规范键不同;
                // 它们被越过不代表用户否定了正确输入,不记负分。
                if skipped
                    .learn_key
                    .as_deref()
                    .is_some_and(|stored| stored != scheme_key.as_str())
                {
                    continue;
                }
                let key = skipped.learn_key.clone().unwrap_or(scheme_key);
                let text = skipped
                    .learn_text
                    .as_deref()
                    .unwrap_or(skipped.text.as_str());
                user.reject(&key, text);
            }
            user.forgive_word(&learn_text);
            user.learn(&learned, &learn_text);
            if cand.groups != 0 {
                user.learn_groups(cand.groups);
            }
            user.learn_transitions(self.last_word.as_deref(), &cand.words);
            user.learn_trigrams(
                self.second_last_word.as_deref(),
                self.last_word.as_deref(),
                &cand.words,
            );
            user.learn_app(&self.host, &cand.words);
        }
        if fixed {
            // 让 `note_events` 把上一条词序切断,而不是把日期/表情记成前词。
            self.last_word = None;
            self.second_last_word = None;
        } else if cand.words.len() >= 2 {
            self.second_last_word = cand.words.get(cand.words.len() - 2).cloned();
            self.last_word = cand.words.last().cloned();
            self.transition_handled += 1;
        } else {
            self.second_last_word = self.last_word.take();
            self.last_word = cand.words.last().cloned();
            self.transition_handled += 1;
        }
        let rest = raw[cut..].trim_start_matches('\'').to_string();
        // 同一次组合里连续选词:把各段拼音与文本拼起来,整串结束时造一个
        // 整句词条。只选一次(整串或单段)时上面已经学过,不重复记。
        let phrase = if self.learning && !fixed {
            match self.phrase.take() {
                Some((mut key, mut text)) => {
                    key.push('\'');
                    key.push_str(&learned);
                    text.push_str(&learn_text);
                    Some((key, text))
                }
                None if !rest.is_empty() => Some((learned, learn_text.clone())),
                None => None,
            }
        } else {
            None
        };
        if rest.is_empty() {
            if let Some((key, text)) = phrase
                && text.chars().count() > 1
            {
                self.user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .learn(&key, &text);
            }
        } else {
            self.phrase = phrase;
        }
        if rest.is_empty() {
            // 规范字形的上下文在这里写入;`note_events` 据此跳过这条提交,
            // 繁体显示也就不会把繁体字带进左邻文本。
            if !learn_text.is_ascii() {
                if self.learning && !fixed {
                    self.user
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .learn_context(&self.context, &learn_text);
                }
                self.push_context(&learn_text);
                // `note_events` 据此跳过这条提交,不再把繁体显示写进上下文。
                self.context_handled += 1;
            }
            self.comp = None;
            self.comp_committed = false;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            // 剩余拼音马上要重算:先把刚上屏的片段并入左邻上下文,下一段的
            // 候选才能看到刚形成的搭配。`note_events` 会跳过这次重复处理。
            if !learn_text.is_ascii() {
                if self.learning && !fixed {
                    self.user
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .learn_context(&self.context, &learn_text);
                }
                self.push_context(&learn_text);
            }
            self.context_handled += 1;
            let comp = self.comp.as_mut()?;
            comp.raw = rest;
            // 选掉前缀后光标仍在剩余拼音末尾,继续按键才会接在后面;
            // 置 0 会让下一个字母插到最前面,变成 `htianqi`。
            comp.cursor = comp.raw.len();
            self.comp_committed = true;
            self.recompute();
        }
        Some(cand.text)
    }
}
