//! 按键状态机:事件管线与键分发。
//!
//! 与 `state`、`input_edit`、`input_commit`、`input_recompute` 分文件只是
//! 职责边界:两边都是 `Engine` 的 inherent impl,共享 crate 根里的私有字段。
//! 跨文件调用的方法显式标为 `pub(crate)`。

use super::*;

impl Engine {
    /// 处理一次按键,返回由此产生的全部事件。
    pub fn process(&mut self, key: Key) -> Vec<Event> {
        // Shift 会先落定中文组合再切模式;上屏文本仍属于切换前的模式。
        let mode = self.mode;
        let mut events = self.process_inner(key);
        self.apply_output(mode, &mut events);
        self.note_events(&events);
        events
    }

    /// 把整个组合按当前切分逐段落定(失焦、别的窗口切全局模式)。
    /// 与空格不同,这里不会只上屏吃前缀的候选而把剩余拼音留在组合里。
    pub fn flush(&mut self) -> Vec<Event> {
        let mut events = self.commit_all();
        self.apply_output(self.mode, &mut events);
        self.note_events(&events);
        events
    }

    /// 上屏事件离开引擎前应用模块转换。默认注册表为空,不进入循环。
    /// `mode` 取事件产生时的模式,Shift 切换前落定的中文不会被当成英文。
    pub(crate) fn apply_output(&self, mode: Mode, events: &mut [Event]) {
        if self.modules.is_empty() {
            return;
        }
        let ctx = module::CommitContext {
            host: &self.host,
            mode,
        };
        for event in events {
            if let Event::Commit(text) = event {
                self.modules.commit(&ctx, text);
            }
        }
    }

    /// 上屏文本进入左邻上下文(ASCII 直通内容不算),同时记住最后一个字符
    /// 是不是数字。直通的数字/字母/标点也在这里更新,后续句号才能判断
    /// 该出半角小数点还是中文句号。
    pub(crate) fn note_events(&mut self, events: &[Event]) {
        for event in events {
            match event {
                Event::Commit(text) => {
                    if self.context_handled > 0 {
                        self.context_handled -= 1;
                    } else if !text.is_ascii() {
                        // 快捷输入是系统生成的数字/日期,不进个人搭配;左邻文本
                        // 仍要更新,让紧随其后的候选看到正确的上下文。
                        if self.learning && self.quick_handled == 0 {
                            self.user
                                .write()
                                .unwrap_or_else(|p| p.into_inner())
                                .learn_context(&self.context, text);
                        }
                        self.push_context(text);
                    }
                    // 候选提交已在 apply_consume 学过词级转移;标点、英文和
                    // 原样上屏会切断词序,下一条候选不再沿用上一个词。
                    if self.transition_handled > 0 {
                        self.transition_handled -= 1;
                    } else {
                        self.last_word = None;
                        self.second_last_word = None;
                    }
                    if self.quick_handled > 0 {
                        self.quick_handled -= 1;
                    }
                    self.after_digit = ends_with_digit(text);
                }
                Event::Passthrough(Key::Digit(_)) => {
                    self.after_digit = true;
                    // 直通内容由宿主直接写进文档,已经断开中文词序。
                    self.last_word = None;
                    self.second_last_word = None;
                }
                Event::Passthrough(_) => {
                    self.after_digit = false;
                    self.last_word = None;
                    self.second_last_word = None;
                }
                // 命令没有上屏文字,词序在这里断开;左邻上下文保持原样。
                Event::Action(_) => {
                    self.after_digit = false;
                    self.last_word = None;
                    self.second_last_word = None;
                }
                Event::Mode(_) => {}
            }
        }
    }

    pub(crate) fn process_inner(&mut self, key: Key) -> Vec<Event> {
        if key == Key::Shift {
            // 首选可能只吃前缀音节;切英文前把整串都落定,不能留下半个组合。
            let mut events = if self.mode == Mode::Chinese {
                self.commit_all()
            } else {
                Vec::new()
            };
            self.mode = if self.mode == Mode::Chinese {
                Mode::English
            } else {
                Mode::Chinese
            };
            events.push(Event::Mode(self.mode));
            return events;
        }
        if self.mode == Mode::English {
            return self.english_events(key);
        }
        if self.comp.is_some() {
            self.process_composing(key)
        } else {
            self.process_idle(key)
        }
    }

    /// 英文态:全角开着时把可打印 ASCII 按键转成全角上屏,其余键照常直通。
    pub(crate) fn english_events(&mut self, key: Key) -> Vec<Event> {
        if !self.fullwidth {
            return vec![Event::Passthrough(key)];
        }
        let text = match key {
            Key::Letter(c) => punct::fullwidth(c),
            Key::Digit(d) => char::from_digit(u32::from(d), 10).and_then(punct::fullwidth),
            Key::Punct(p) => punct::fullwidth(p),
            Key::Space => Some('\u{3000}'),
            _ => None,
        };
        match text {
            Some(c) => vec![Event::Commit(c.to_string())],
            None => vec![Event::Passthrough(key)],
        }
    }

    pub(crate) fn process_idle(&mut self, key: Key) -> Vec<Event> {
        match key {
            Key::Letter(c) if c.is_ascii_lowercase() => {
                self.begin_composition(c);
                Vec::new()
            }
            // 全角下中文态直接上大写字母、数字与空格;小写字母仍起拼音。
            Key::Letter(c) if self.fullwidth => {
                let text = punct::fullwidth(c).map_or_else(|| c.to_string(), String::from);
                vec![Event::Commit(text)]
            }
            Key::Digit(d) if self.fullwidth => {
                let text = char::from_digit(u32::from(d), 10)
                    .and_then(punct::fullwidth)
                    .map_or_else(|| d.to_string(), String::from);
                vec![Event::Commit(text)]
            }
            Key::Space if self.fullwidth => vec![Event::Commit('\u{3000}'.to_string())],
            Key::Punct(p) => self.punct_events(p),
            _ => vec![Event::Passthrough(key)],
        }
    }

    pub(crate) fn begin_composition(&mut self, c: char) {
        self.comp = Some(Composition {
            raw: c.to_string(),
            cursor: 1,
            display: String::new(),
            seg_spans: Vec::new(),
        });
        self.comp_committed = false;
        self.recompute();
    }

    pub(crate) fn process_composing(&mut self, key: Key) -> Vec<Event> {
        match key {
            Key::Letter(c) if c.is_ascii_lowercase() => {
                // v 表达式里 x 是乘号;组合已满时退回普通字母,先落定再起新组合。
                if c == 'x' && self.quick_claims(&Key::Letter(c)) && self.compose_char('x') {
                    return Vec::new();
                }
                if self
                    .comp
                    .as_ref()
                    .is_some_and(|comp| comp.raw.len() >= MAX_RAW)
                {
                    // 组合到达上限:先把当前输入按首选落定,再用这一键起新组合。
                    // 直接丢弃会让长句末尾的按键无声消失,文档与用户预期错位。
                    let events = self.commit_all();
                    self.phrase = None;
                    self.begin_composition(c);
                    return events;
                }
                if let Some(comp) = self.comp.as_mut() {
                    comp.raw.insert(comp.cursor, c);
                    comp.cursor += 1;
                }
                self.recompute();
                Vec::new()
            }
            Key::Punct('\'') => {
                self.try_insert_apostrophe();
                Vec::new()
            }
            // 方案自己的符号键:微软/搜狗双拼的 `;` 是 ing,辅助码的 `` ` ``
            // 进入辅码段。只在组合中生效,空闲时仍是普通标点。
            Key::Punct(p) if self.scheme.accepts_char(p) => {
                if self.compose_char(p) {
                    Vec::new()
                } else {
                    self.commit_all_then(p.to_string())
                }
            }
            // 问字的十六进制常写成大写;Shift+字母仍属于码点,不落定组合。
            Key::Letter(c) if c.is_ascii_hexdigit() && self.quick_claims(&Key::Letter(c)) => {
                if self.compose_char(c.to_ascii_lowercase()) {
                    Vec::new()
                } else {
                    self.commit_all_then(c.to_string())
                }
            }
            // 空格确认当前高亮:高亮在动作候选上时执行命令,否则照旧上屏文字。
            Key::Space => {
                let idx = self.hl.min(self.cands.len().saturating_sub(1));
                self.take_action(idx).map_or_else(
                    || {
                        self.commit_highlighted()
                            .map(|t| vec![Event::Commit(t)])
                            .unwrap_or_default()
                    },
                    |id| vec![Event::Action(id)],
                )
            }
            Key::Enter => {
                let raw = self.comp.take().map(|c| c.raw).unwrap_or_default();
                self.comp_committed = false;
                self.phrase = None;
                self.cands.clear();
                self.rev = self.rev.wrapping_add(1);
                vec![Event::Commit(raw)]
            }
            Key::Esc => {
                self.clear_comp();
                Vec::new()
            }
            Key::Backspace => {
                self.delete_char(true);
                Vec::new()
            }
            Key::Delete => {
                self.delete_char(false);
                Vec::new()
            }
            Key::SegmentBackspace => {
                self.delete_segment(true);
                Vec::new()
            }
            Key::SegmentDelete => {
                self.delete_segment(false);
                Vec::new()
            }
            Key::SegmentLeft => {
                self.move_segment(-1);
                Vec::new()
            }
            Key::SegmentRight => {
                self.move_segment(1);
                Vec::new()
            }
            Key::DeleteCandidate => {
                self.forget_highlighted();
                Vec::new()
            }
            Key::Left => {
                self.move_cursor_char(-1);
                Vec::new()
            }
            Key::Right => {
                self.move_cursor_char(1);
                Vec::new()
            }
            Key::Home => {
                if let Some(comp) = self.comp.as_mut() {
                    comp.cursor = 0;
                }
                Vec::new()
            }
            Key::End => {
                if let Some(comp) = self.comp.as_mut() {
                    comp.cursor = comp.raw.len();
                }
                Vec::new()
            }
            Key::Up => {
                self.move_highlight(-1);
                Vec::new()
            }
            Key::Down => {
                self.move_highlight(1);
                Vec::new()
            }
            Key::PageUp | Key::PageDown => {
                self.page_by(if key == Key::PageUp { -1 } else { 1 });
                Vec::new()
            }
            // 模块触发会话(v 表达式、u 问字)认领的键:当输入组合,不当
            // 标点。组合已满时落定后再上屏。
            Key::Punct(p) if self.quick_claims(&Key::Punct(p)) => {
                if self.compose_char(p) {
                    Vec::new()
                } else {
                    self.commit_all_then(p.to_string())
                }
            }
            // 邮箱:用户名后接 @,此后字母数字与点继续筛常见域。
            Key::Punct('@')
                if self
                    .comp
                    .as_ref()
                    .is_some_and(|c| address::email_at_ok(&c.raw)) =>
            {
                if self.compose_char('@') {
                    Vec::new()
                } else {
                    self.commit_all_then("@".to_string())
                }
            }
            // 邮箱域名与网址里的小数点属于地址;地址之外仍恢复普通标点。
            Key::Punct('.') if self.comp.as_ref().is_some_and(|c| address::dot_ok(&c.raw)) => {
                if self.compose_char('.') {
                    Vec::new()
                } else {
                    self.commit_all_then(".".to_string())
                }
            }
            // 网址里的路径斜杠与连字符并入组合;邮箱域不收,筛域用不到。
            Key::Punct(p @ ('/' | '-'))
                if self.comp.as_ref().is_some_and(|c| address::in_url(&c.raw)) =>
            {
                if self.compose_char(p) {
                    Vec::new()
                } else {
                    self.commit_all_then(p.to_string())
                }
            }
            // 微软拼音习惯:组合中 -/= 翻页;只有一页时退回标点。
            Key::Punct('-') if self.has_pages() => {
                self.page_by(-1);
                Vec::new()
            }
            Key::Punct('=') if self.has_pages() => {
                self.page_by(1);
                Vec::new()
            }
            // 快捷会话(v 表达式、u 问字)里数字是输入的一部分,不再当候选
            // 序号;地址形态下数字继续筛域名。
            Key::Digit(d) if self.quick_claims(&Key::Digit(d)) => self.compose_digit(d),
            Key::Digit(d) if self.address_mode() => self.compose_digit(d),
            Key::Digit(d) => self.select_digit(d),
            Key::Punct(p) => {
                // 标点把句子切开了,整串落定后再上屏标点;已选片段不再拼成整句词条。
                let mut events = self.commit_all();
                self.phrase = None;
                // 刚上屏的是中文候选;组句前的数字不再影响这个句号。
                self.after_digit = false;
                // 没有中文映射(或关了中文标点)时按 ASCII 上屏;
                // 直通会让应用在组句未收时插入字符,文档与引擎就错位了。
                let mapped = if self.cfg.chinese_punct {
                    punct::map_with(
                        &self.cfg.punct,
                        p,
                        &mut self.dq_open,
                        &mut self.sq_open,
                        &mut self.after_digit,
                    )
                } else {
                    None
                };
                let text = mapped.unwrap_or_else(|| {
                    if self.fullwidth {
                        punct::fullwidth(p).map_or_else(|| p.to_string(), String::from)
                    } else {
                        p.to_string()
                    }
                });
                events.push(Event::Commit(text));
                events
            }
            // 大写字母(Shift+字母)在这里落定整串组合,再由调用方写进文档;
            // 返回直通会让应用在组合未收时插字,留下无法收拾的内联拼音。
            Key::Letter(c) => {
                let mut events = self.commit_all();
                self.phrase = None;
                let text = if self.fullwidth {
                    punct::fullwidth(c).map_or_else(|| c.to_string(), String::from)
                } else {
                    c.to_string()
                };
                events.push(Event::Commit(text));
                events
            }
            // Shift 在 process 入口已处理,组合态里不会再遇到。
            Key::Shift => Vec::new(),
        }
    }

    /// 标点:开中文标点则上屏全角,否则全角模式下上屏全角 ASCII,再否则直通。
    pub(crate) fn punct_events(&mut self, p: char) -> Vec<Event> {
        if self.cfg.chinese_punct
            && let Some(s) = punct::map_with(
                &self.cfg.punct,
                p,
                &mut self.dq_open,
                &mut self.sq_open,
                &mut self.after_digit,
            )
        {
            return vec![Event::Commit(s)];
        }
        if self.fullwidth
            && let Some(c) = punct::fullwidth(p)
        {
            return vec![Event::Commit(c.to_string())];
        }
        vec![Event::Passthrough(Key::Punct(p))]
    }

    pub(crate) fn select_digit(&mut self, d: u8) -> Vec<Event> {
        let ps = self.cfg.page_size.max(1);
        let start = self.page * ps;
        let end = (start + ps).min(self.cands.len());
        if d >= 1 && d as usize <= end - start {
            let idx = start + d as usize - 1;
            // 数字键选中动作候选:执行命令,不上屏文字,组合清空。
            if let Some(id) = self.take_action(idx) {
                return vec![Event::Action(id)];
            }
            match self.apply_consume(idx) {
                Some(t) => vec![Event::Commit(t)],
                None => vec![Event::Passthrough(Key::Digit(d))],
            }
        } else {
            // 当前页没有这个序号:先收组句再上屏数字。组合中直通会让应用
            // 把数字插进内联拼音里,引擎与文档随即错位。
            let mut events = self.commit_all();
            self.phrase = None;
            events.push(Event::Commit(d.to_string()));
            events
        }
    }

    /// 快捷模式数字键进入组合;组合已满时退回候选序号,不静默吞键。
    pub(crate) fn compose_digit(&mut self, d: u8) -> Vec<Event> {
        if self
            .comp
            .as_ref()
            .is_some_and(|comp| comp.raw.len() >= MAX_RAW)
        {
            return self.select_digit(d);
        }
        let Some(c) = char::from_digit(u32::from(d), 10) else {
            return self.select_digit(d);
        };
        if self.compose_char(c) {
            Vec::new()
        } else {
            self.select_digit(d)
        }
    }

    /// 组合是否处于地址形态:邮箱(含 @)或网址(www. 开头)。
    pub(crate) fn address_mode(&self) -> bool {
        let Some(comp) = self.comp.as_ref() else {
            return false;
        };
        address::in_email(&comp.raw) || address::in_url(&comp.raw)
    }

    /// 模块触发会话(v 表达式、u 问字)是否认领该键;认领的键按输入组合
    /// 处理,不当标点、翻页或候选序号。触发前缀未命中时不进入任何模块。
    fn quick_claims(&self, key: &Key) -> bool {
        let Some(comp) = self.comp.as_ref() else {
            return false;
        };
        self.modules.claims_key(&comp.raw, key)
    }
}
