//! 候选重算与动作候选注入。任何组合变化后的唯一重算入口在这里。
//!
//! 与 `input`、`state` 等分文件只是职责边界:`Engine` 的 inherent impl,
//! 共享 crate 根里的私有字段。跨文件调用的方法显式标为 `pub(crate)`。

use super::*;

/// 同一触发词下用户选了这么多次字面原文后,不再注入动作候选。
/// 计数借用户词库的同键负分,按周衰减,用户改选动作候选即可回落。
const ACTION_SUPPRESS: u32 = 2;

impl Engine {
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
        let raw = comp.raw.clone();
        // 方案把击键展开成规范全拼;词图只认展开后的串,候选区间再映射回击键。
        let expansion = self.scheme.expand(&raw);
        let base_len = expansion.aux_start.unwrap_or(raw.len());
        let user = self.user.read().unwrap_or_else(|p| p.into_inner());
        let mut built = query::build(query::BuildArgs {
            dict: &self.dict,
            user: &user,
            fuzzy: &self.cfg.fuzzy,
            gram: self.gram.as_deref(),
            cache: &mut self.gram_cache,
            context: &context,
            host: &self.host,
            previous2: self.second_last_word.as_deref(),
            previous: self.last_word.as_deref(),
            raw: &expansion.query,
            quick_trigger: self.modules.raw_trigger(&expansion.query),
        });
        // 候选的 spans 目前指向查询串;先换回用户实际敲的击键。
        for cand in &mut built.cands {
            for span in &mut cand.spans {
                *span = (
                    expansion.raw_at.get(span.0).copied().unwrap_or(base_len),
                    expansion.raw_at.get(span.1).copied().unwrap_or(base_len),
                );
            }
        }
        // 辅助码只对覆盖整个基础输入的候选生效:辅码段要随候选一起上屏。
        // `full` 是核心对「吃完整串」的判定,比按区间末尾比较更稳:
        // 末尾带隔音符或双拼末键不完整时,原始字节长度并不等于音节边界。
        if let Some(filter) = &expansion.filter {
            let mut fallback = built.cands.clone();
            fallback.retain(|cand| cand.full);
            built
                .cands
                .retain(|cand| cand.full && filter.matches(&cand.text));
            if built.cands.is_empty() {
                built.cands = fallback;
            }
            for cand in &mut built.cands {
                if let Some(span) = cand.spans.last_mut() {
                    span.1 = raw.len();
                }
            }
        } else if expansion.aux_start.is_some() {
            // 只敲了分隔符还没敲辅码:保留完整候选,别把 `` ` `` 留在组合里。
            built.cands.retain(|cand| cand.full);
            for cand in &mut built.cands {
                if let Some(span) = cand.spans.last_mut() {
                    span.1 = raw.len();
                }
            }
        }
        let now = self.now.unwrap_or_else(CivilTime::now_utc);
        // 邮箱/网址候选排在「可信的整词」之后,但越过未打完的前缀和简拼
        // 硬拼出来的整句;日期/时间等快捷项由快捷模块在模块链里插入。
        let address = address::candidates(&raw);
        if !address.is_empty() {
            built
                .cands
                .retain(|cand| !address.iter().any(|a| a.text == cand.text));
            let keep_first = built
                .cands
                .first()
                .is_some_and(|cand| cand.full && !cand.sentence && (cand.trusted || cand.learned));
            let position = if keep_first { 1 } else { 0 };
            for cand in address.into_iter().rev() {
                built.cands.insert(position, cand);
            }
        }
        // 置顶词:整个组合键下右键置顶过的词,直接放首位;重排与快捷项都
        // 让位。置顶槽按去掉隔音符的字母串索引;旧版本按原始击键存键,再查
        // 两次兼容。
        let pin_key = expansion.key.replace('\'', "");
        if let Some(word) = user
            .pinned(&pin_key)
            .or_else(|| user.pinned(&expansion.key))
            .or_else(|| user.pinned(&raw))
            .filter(|word| !word.starts_with('!'))
        {
            built.cands.retain(|cand| cand.text != word);
            let mut cand = Candidate::whole(raw.len(), word.to_string(), 1.0e12);
            cand.pinned = true;
            built.cands.insert(0, cand);
        }
        // 内置与宿主模块在核心候选、地址与置顶之后运行:快捷输入、短语、
        // 联想、表情、英文混输、简繁转换按注册顺序增删改候选。默认空注册表
        // 不进这个分支。
        if !self.modules.is_empty() {
            let module_ctx = module::CandidateContext {
                raw: &raw,
                query: &expansion.query,
                scheme: self.scheme.manifest().id,
                quick_trigger: self.modules.raw_trigger(&raw),
                context: &context,
                host: &self.host,
                now,
                previous: self.last_word.as_deref(),
                previous2: self.second_last_word.as_deref(),
                user: &user,
            };
            self.modules.candidates(&module_ctx, &mut built.cands);
        }
        // 动作候选:触发词整串完全匹配时把命令放进候选;默认不进首位,
        // 字面原文候选补在这一步,空格仍照旧上屏文字。
        if let Some(action) = self.action.clone() {
            inject_action(&mut built.cands, &action, &raw, &user);
        }
        comp.display = built
            .cands
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_else(|| raw.clone());
        comp.seg_spans = built
            .cands
            .first()
            .map(|c| c.spans.clone())
            .unwrap_or_else(|| expansion.segments.clone());
        self.cands = built.cands;
        // 候选换了一批:页号与高亮回到第一条。翻到第二页再继续打字时
        // 旧页号可能已经越界,候选窗会空着并显示 3/1 这样的页码。
        self.hl = 0;
        self.page = 0;
    }
}

/// 把动作候选注入当前候选表。触发词必须与整串输入完全一致(隔音符不计);
/// 用户多次选字面原文后抑制。字面原文候选始终保留,空格照旧上屏文字。
fn inject_action(cands: &mut Vec<Candidate>, action: &Action, raw: &str, user: &UserDict) {
    let key = raw.replace('\'', "");
    if action.trigger != key {
        return;
    }
    let pin = format!("!{}", action.id);
    if user.reject_effective(&key, &pin) >= ACTION_SUPPRESS {
        return;
    }
    // 没有任何候选等于原文时补一条页尾字面候选:引擎与扩展都跳过了它,
    // 用户仍有不上屏、直接要字母的退路。
    if !cands.iter().any(|cand| cand.text == raw) {
        let mut literal = Candidate::whole(raw.len(), raw.to_string(), -1.0e12);
        literal.words.clear();
        literal.no_learn = true;
        literal.hint = Some("原文".to_string());
        cands.push(literal);
    }
    let mut cand = Candidate::whole(raw.len(), action.title.clone(), 1.0e12);
    cand.words.clear();
    cand.no_learn = true;
    cand.hint = Some("命令".to_string());
    cand.action = Some(action.id.clone());
    if user.pinned(&key).is_some_and(|word| word == pin.as_str()) {
        cand.pinned = true;
        cands.insert(0, cand);
    } else {
        // 默认不进首位;没有其他候选时排页尾。
        let at = 1.min(cands.len());
        cands.insert(at, cand);
    }
}
