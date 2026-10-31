//! 下一词联想扩展。
//!
//! 只用本机用户词库里的词级转移与三词窗口:上一词之后用户实际接过的词,
//! 在下一段输入是它的拼音前缀时作为「联想」候选出现。没有云端请求,也没有
//! 冷启动语料;用户第一次接某个词后,下一次就能命中。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};

/// 每次组合最多补几条联想,避免把首页灌满。
const MAX_SUGGESTIONS: usize = 3;

/// 外部联想来源:给前词序列,返回 (词, 全拼)。内置(用户词库搭配)是
/// 默认;外部来源的结果排在内置联想之后,拼音同样要过前缀过滤。同步
/// 契约:调用发生在按键路径上,实现必须亚毫秒级(本地小表、预取好的
/// 缓存);慢模型请在旁路进程自己预取,把现成结果放进来。
pub trait PredictSource: Send + Sync {
    fn predict(&self, previous: &[&str], limit: usize) -> Vec<(String, String)>;
}

/// 下一词联想模块。
pub struct Prediction {
    enabled: AtomicBool,
    source: Mutex<Option<Arc<dyn PredictSource>>>,
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
            source: Mutex::new(None),
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

    /// 装外部联想来源(提供者缝);`None` 回到纯内置。
    pub fn set_source(&self, source: Option<Arc<dyn PredictSource>>) {
        *self.source.lock().unwrap_or_else(|p| p.into_inner()) = source;
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
        // 外部来源(提供者缝)排在内置联想之后,拼音同样要过前缀过滤。
        if inserted < MAX_SUGGESTIONS {
            let words: Vec<&str> = [ctx.previous, ctx.previous2]
                .into_iter()
                .flatten()
                .collect();
            let source = self.source.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(source) = source.as_ref() {
                let externals = source.predict(&words, MAX_SUGGESTIONS - inserted);
                merge_external(cands, ctx.raw.len(), &query, externals, inserted);
            }
        }
    }
}

/// 把外部联想并进候选,返回累计插入条数;`inserted` 是已插入的内置条数。
/// 同词不重复,拼音要过前缀过滤,分数压在内置联想(1.0e8)之下。
fn merge_external(
    cands: &mut Vec<Candidate>,
    raw_len: usize,
    query: &str,
    externals: Vec<(String, String)>,
    mut inserted: usize,
) -> usize {
    for (word, pinyin) in externals {
        if inserted >= MAX_SUGGESTIONS {
            break;
        }
        if cands.iter().any(|cand| cand.text == word) {
            continue;
        }
        if !matches_key(&pinyin, query) {
            continue;
        }
        let mut cand = Candidate::whole(raw_len, word, 1.0e7 - inserted as f64);
        cand.completion = true;
        cand.hint = Some("联想".to_string());
        let at = (inserted + 1).min(cands.len());
        cands.insert(at, cand);
        inserted += 1;
    }
    inserted
}

#[cfg(test)]
#[path = "predict_tests.rs"]
mod tests;

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
