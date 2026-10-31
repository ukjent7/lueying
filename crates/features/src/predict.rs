//! 下一词联想扩展。
//!
//! 只用本机用户词库里的词级转移与三词窗口:上一词之后用户实际接过的词,
//! 在下一段输入是它的拼音前缀时作为「联想」候选出现。没有云端请求,也没有
//! 冷启动语料;用户第一次接某个词后,下一次就能命中。

use std::sync::atomic::{AtomicBool, Ordering};

use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};

/// 每次组合最多补几条联想,避免把首页灌满。
const MAX_SUGGESTIONS: usize = 3;

/// 下一词联想模块。
pub struct Prediction {
    enabled: AtomicBool,
}

impl Prediction {
    /// 默认启用。
    pub fn new() -> Self {
        Self::with_enabled(true)
    }

    /// 指定初始开关;宿主启动时按配置构造。
    pub fn with_enabled(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
        }
    }

    /// 设置页切换;常驻服务持有同一个实例,已有会话立即跟随。
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// 当前是否启用。
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

impl Default for Prediction {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Prediction {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "prediction",
            name: "下一词联想",
            version: env!("CARGO_PKG_VERSION"),
            capabilities: &[Capability::Candidates],
            triggers: &[],
        }
    }

    fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        if !self.is_enabled() {
            return;
        }
        // 辅助码组合不掺联想:辅码正在筛拼音候选,插进来的词没有字形依据。
        if ctx.raw.contains('`') {
            return;
        }
        let query = normalize(ctx.query);
        if query.is_empty() {
            return;
        }
        let mut predicted = ctx.user.predict_next(ctx.previous, 24);
        if predicted.is_empty() {
            predicted = ctx.user.predict_after(ctx.previous2, ctx.previous, 24);
        }
        let mut inserted = 0usize;
        for (word, _count) in predicted {
            let Some(key) = ctx.user.key_for_word(&word) else {
                continue;
            };
            if !matches_key(key, &query) {
                continue;
            }
            // 核心候选里已经有同一个词时不再补一条「联想」。
            if cands.iter().any(|cand| cand.text == word) {
                continue;
            }
            let mut cand = Candidate::whole(ctx.raw.len(), word, 1.0e8 - inserted as f64);
            cand.learn_key = Some(key.to_string());
            cand.completion = true;
            cand.hint = Some("联想".to_string());
            let at = (inserted + 1).min(cands.len());
            cands.insert(at, cand);
            inserted += 1;
            if inserted >= MAX_SUGGESTIONS {
                break;
            }
        }
    }
}

/// 当前输入是否是联想词拼音的前缀。全拼按字母前缀,简拼按各音节首字母。
fn matches_key(key: &str, query: &str) -> bool {
    let plain = key.replace('\'', "");
    if plain.starts_with(query) {
        return true;
    }
    let initials: String = key
        .split('\'')
        .filter_map(|syllable| syllable.chars().next())
        .collect();
    initials.starts_with(query)
}

fn normalize(query: &str) -> String {
    query
        .chars()
        .filter(char::is_ascii_alphabetic)
        .flat_map(char::to_lowercase)
        .collect()
}
