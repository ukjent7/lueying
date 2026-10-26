//! 按键状态机、组合编辑、上屏与候选重算。
//!
//! 与 `state` 分文件只是职责边界:两边都是 `Engine` 的 inherent impl,共享
//! crate 根里的私有字段。跨文件调用的方法显式标为 `pub(crate)`。

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
                if c == 'x' && self.expression_body().is_some() && self.compose_char('x') {
                    return Vec::new();
                }
                if self
                    .comp
                    .as_ref()
                    .is_some_and(|comp| comp.raw.len() >= MAX_RAW)
                {
                    // 组合到达上限:先把当前输入按首选落定,再用这一键起新组合。
                    // 直接丢弃会让长句末尾的按键无声消失,文档与用户预期错位。
                    let mut events = self.commit_all();
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
            // 问字的十六进制常写成大写;Shift+字母仍属于码点,不落定组合。
            Key::Letter(c) if self.unicode_body().is_some() && c.is_ascii_hexdigit() => {
                if self.compose_char(c.to_ascii_lowercase()) {
                    Vec::new()
                } else {
                    let mut events = self.commit_all();
                    self.phrase = None;
                    events.push(Event::Commit(c.to_string()));
                    events
                }
            }
            Key::Space => self
                .commit_highlighted()
                .map(|t| vec![Event::Commit(t)])
                .unwrap_or_default(),
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
            // v 表达式里的运算符属于输入;组合已满时落定后再上屏。
            Key::Punct(p) if self.expression_body().is_some() && quick::is_operator(p) => {
                if self.compose_char(p) {
                    Vec::new()
                } else {
                    let mut events = self.commit_all();
                    self.phrase = None;
                    events.push(Event::Commit(p.to_string()));
                    events
                }
            }
            // u 问字:第一个 + 表示显式码点;之后的 + 恢复普通标点。
            Key::Punct('+') if self.unicode_body().is_some_and(|body| body.is_empty()) => {
                if self.compose_char('+') {
                    Vec::new()
                } else {
                    let mut events = self.commit_all();
                    self.phrase = None;
                    events.push(Event::Commit("+".to_string()));
                    events
                }
            }
            // v 模式的小数点属于当前数字段;这一段已有小数点时恢复普通标点。
            Key::Punct('.') if self.expression_dot_mode() => {
                if self.compose_char('.') {
                    Vec::new()
                } else {
                    let mut events = self.commit_all();
                    self.phrase = None;
                    events.push(Event::Commit(".".to_string()));
                    events
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
            // v 表达式与 u 问字里数字是输入的一部分,不再当候选序号。
            Key::Digit(d) if self.expression_body().is_some() => self.compose_digit(d),
            Key::Digit(d) if self.unicode_body().is_some() => self.compose_digit(d),
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
                    punct::map(
                        p,
                        &mut self.dq_open,
                        &mut self.sq_open,
                        &mut self.after_digit,
                    )
                } else {
                    None
                };
                let text = mapped.map_or_else(
                    || {
                        if self.fullwidth {
                            punct::fullwidth(p).map_or_else(|| p.to_string(), String::from)
                        } else {
                            p.to_string()
                        }
                    },
                    |s| s.to_string(),
                );
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
            && let Some(s) = punct::map(
                p,
                &mut self.dq_open,
                &mut self.sq_open,
                &mut self.after_digit,
            )
        {
            return vec![Event::Commit(s.to_string())];
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

    /// 组合是否处于 v 表达式模式:字母 v 后只跟数字、运算符与小数点。
    pub(crate) fn expression_body(&self) -> Option<&str> {
        let raw = self.comp.as_ref()?.raw.as_str();
        let body = raw.strip_prefix('v')?;
        body.chars().all(quick::is_expression_char).then_some(body)
    }

    /// 组合是否处于 u 问字模式:字母 u 后是至多六位十六进制,可带前导 `+`。
    pub(crate) fn unicode_body(&self) -> Option<&str> {
        let raw = self.comp.as_ref()?.raw.as_str();
        quick::unicode_body(raw)
    }

    /// 当前小数点属于表达式里的数字段;这段还没有小数点。
    ///
    /// 点分隔的日期例外:`2006.12` 后的点要能补成 `2006.12.25`。普通小数
    /// 后面再敲点仍恢复标点,避免把句号吞进组合。
    pub(crate) fn expression_dot_mode(&self) -> bool {
        let Some(body) = self.expression_body() else {
            return false;
        };
        if body.is_empty() {
            return false;
        }
        let segment = body
            .rsplit(|c: char| !(c.is_ascii_digit() || c == '.'))
            .next()
            .unwrap_or("");
        if !segment.contains('.') {
            return true;
        }
        let mut parts = body.split('.');
        let year = parts.next().unwrap_or("");
        let month = parts.next().unwrap_or("");
        parts.next().is_none()
            && year.len() == 4
            && year.bytes().all(|b| b.is_ascii_digit())
            && (1..=2).contains(&month.len())
            && month.bytes().all(|b| b.is_ascii_digit())
            && month
                .parse::<u32>()
                .is_ok_and(|month| (1..=12).contains(&month))
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

    /// 逐段落定整个组合;没有候选时上屏原文。返回按序的上屏事件。
    pub(crate) fn commit_all(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(text) = self.commit_highlighted() {
            events.push(Event::Commit(text));
        }
        events
    }

    /// 上屏高亮候选;无候选时上屏原文。
    pub(crate) fn commit_highlighted(&mut self) -> Option<String> {
        self.comp.as_ref()?;
        if self.cands.is_empty() {
            let raw = self.comp.take().map(|c| c.raw);
            self.comp_committed = false;
            self.phrase = None;
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
            return raw;
        }
        self.apply_consume(self.hl.min(self.cands.len() - 1))
    }

    /// 应用第 idx 个候选:上屏文本,吃掉 consume 个音节,剩余留作组合并学习。
    pub(crate) fn apply_consume(&mut self, idx: usize) -> Option<String> {
        let cand = self.cands.get(idx)?.clone();
        let cut = cand.spans.get(cand.consume - 1).map(|s| s.1)?;
        let raw = self.comp.as_ref()?.raw.clone();
        // 快捷候选不是词:不写用户词库、不学组级适应,也不把日期/金额
        // 当作用词接进词级转移。它们也不参与同一次组合的自动造词。
        if cand.quick {
            self.phrase = None;
            self.quick_handled += 1;
        }
        // 用户词库的键必须和查询时的音节键一致(ni'hao),不能存原始串(nihao),
        // 否则多音节词学完也匹配不上。
        let learned = if cand.quick {
            String::new()
        } else {
            cand.learn_key.clone().unwrap_or_else(|| {
                cand.spans
                    .iter()
                    .take(cand.consume)
                    .map(|&(s, e)| lueying_dict::syllables::canonical(&raw[s..e]))
                    .collect::<Vec<_>>()
                    .join("'")
            })
        };
        if self.learning && !cand.quick {
            let mut user = self.user.write().unwrap_or_else(|p| p.into_inner());
            // 同键下被越过的候选记一次小负分。只统计吃掉同样多音节的候选:
            // 用户改选前缀词上屏时,不能被当成否定了整句读法。手误纠正的候选
            // 是推测出来的键,不拿它给正确输入留下负分。
            for skipped in self.cands.iter().take(idx) {
                if skipped.quick || skipped.learn_key.is_some() || skipped.consume != cand.consume {
                    continue;
                }
                let key = skipped
                    .learn_key
                    .clone()
                    .unwrap_or_else(|| query::spans_key(&raw, &skipped.spans));
                user.reject(&key, &skipped.text);
            }
            user.forgive_word(&cand.text);
            user.learn(&learned, &cand.text);
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
        if cand.quick {
            // 让 `note_events` 把上一条词序切断,而不是把日期记成前词。
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
        let phrase = if self.learning && !cand.quick {
            match self.phrase.take() {
                Some((mut key, mut text)) => {
                    key.push('\'');
                    key.push_str(&learned);
                    text.push_str(&cand.text);
                    Some((key, text))
                }
                None if !rest.is_empty() => Some((learned, cand.text.clone())),
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
            self.comp = None;
            self.comp_committed = false;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            // 剩余拼音马上要重算:先把刚上屏的片段并入左邻上下文,下一段的
            // 候选才能看到刚形成的搭配。`note_events` 会跳过这次重复处理。
            if !cand.text.is_ascii() {
                if self.learning {
                    self.user
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .learn_context(&self.context, &cand.text);
                }
                self.push_context(&cand.text);
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
        let spans = segment::first_path(&comp.raw);
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
        let spans = segment::first_path(&raw);
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

    /// 重算候选与内联显示。任何组合变化后必须调用。
    pub(crate) fn recompute(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        self.hl = 0;
        self.page = 0;
        let Some(comp) = self.comp.as_mut() else {
            self.cands.clear();
            return;
        };
        // 缓存只增不减会随输入历史膨胀;超过上限就整体丢弃,后续按键会重新填充。
        if self.gram_cache.len() > 4096 {
            self.gram_cache.clear();
        }
        let context = self.context.clone();
        let user = self.user.read().unwrap_or_else(|p| p.into_inner());
        let mut built = query::build(
            &self.dict,
            &user,
            &self.cfg.fuzzy,
            self.gram.as_deref(),
            &mut self.gram_cache,
            &context,
            &self.host,
            self.second_last_word.as_deref(),
            self.last_word.as_deref(),
            &comp.raw,
        );
        let now = self.now.unwrap_or_else(CivilTime::now_utc);
        // 日期/时间/金额排在「可信的整词」之后,但越过未打完的前缀和
        // 简拼硬拼出来的整句:`sj` 仍以「时间」为首选,`rq`/`xq` 直接出快捷项。
        let quick = quick::candidates(&comp.raw, now);
        if !quick.is_empty() {
            built
                .cands
                .retain(|cand| !quick.iter().any(|q| q.text == cand.text));
            let keep_first = built.cands.first().is_some_and(|cand| {
                cand.full && !cand.sentence && (cand.trusted || cand.user_freq > 0)
            });
            let position = if keep_first { 1 } else { 0 };
            for cand in quick.into_iter().rev() {
                built.cands.insert(position, cand);
            }
        } else if let Some((at, quick)) = quick::keyword(&built.cands, &comp.raw, now) {
            // 全拼打出了「日期/时间/星期」:写法紧跟这个词,不抢它的首选。
            for cand in quick.into_iter().rev() {
                built.cands.insert(at + 1, cand);
            }
        }
        // 可选模块在核心候选与快捷输入之后运行;默认空注册表不进入这个分支。
        if !self.modules.is_empty() {
            let module_ctx = module::CandidateContext {
                raw: &comp.raw,
                context: &context,
                host: &self.host,
                now,
            };
            self.modules.candidates(&module_ctx, &mut built.cands);
        }
        comp.display = built
            .cands
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_else(|| comp.raw.clone());
        comp.seg_spans = built
            .cands
            .first()
            .map(|c| c.spans.clone())
            .unwrap_or_else(|| segment::first_path(&comp.raw));
        self.cands = built.cands;
        // 候选换了一批:页号与高亮回到第一条。翻到第二页再继续打字时
        // 旧页号可能已经越界,候选窗会空着并显示 3/1 这样的页码。
        self.hl = 0;
        self.page = 0;
    }
}
