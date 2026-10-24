//! 本地神经重排:把引擎候选交给字符级语言模型重新排序。
//!
//! 采用 metasequoia 参考实现的门控:词典整键精确命中不覆盖、只比较回答整个
//! 按键的候选、最多九条。4M 走同步按键路径,25M 只在下文 [`debounce`] 的停顿
//! 槽位运行;两者共用同一套 [`Fact`] 契约。

pub mod debounce;

use std::sync::Arc;

pub use chinese_ime_lm::ModelError;
use chinese_ime_lm::{CandidateFacts, Reranker, SentenceModel};

/// 加载后的模型。权重只读,克隆只增加引用计数,多个会话共享一份。
#[derive(Clone)]
pub struct Model {
    inner: Arc<SentenceModel>,
}

impl Model {
    pub fn load(bytes: &[u8]) -> Result<Self, ModelError> {
        Ok(Self {
            inner: Arc::new(SentenceModel::load(bytes)?),
        })
    }

    /// 模型能接受的上下文窗口(字符数)。
    pub fn context_length(&self) -> usize {
        self.inner.context_length()
    }

    /// 权重许可(SPDX)。空串表示文件没有声明。
    pub fn license(&self) -> &str {
        self.inner.license()
    }

    /// 训练语料署名。
    pub fn attribution(&self) -> &str {
        self.inner.attribution()
    }
}

/// 一条候选相对当前按键的事实。两者都只有引擎知道,不能从候选文本推断。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fact {
    /// 候选覆盖整个组合输入,而不是前缀词或联想补全。
    pub answers_key: bool,
    /// 词典对用户实际键入的整键给出的精确命中,值得让模型让位。
    pub trusted_dictionary_hit: bool,
}

/// 一个会话独享的推理状态:跨按键前缀缓存和候选续算都放在这里。
pub struct Rerank {
    inner: Reranker,
}

impl Rerank {
    pub fn new(model: &Model) -> Self {
        Self {
            inner: Reranker::new(Arc::clone(&model.inner)),
        }
    }

    /// 返回模型建议移到首位的候选下标;`None` 表示保持引擎顺序。
    ///
    /// `texts` 与 `facts` 必须一一对应,长度不一致时放弃决策。
    pub fn decide(&mut self, context: &str, texts: &[&str], facts: &[Fact]) -> Option<usize> {
        if texts.len() != facts.len() {
            return None;
        }
        self.inner
            .best_where(context, texts, |index| CandidateFacts {
                answers_key: facts[index].answers_key,
                trusted_dictionary_hit: facts[index].trusted_dictionary_hit,
            })
    }

    /// 每条候选的每字符平均对数概率,按输入顺序返回。评测与诊断用。
    pub fn scores(&mut self, context: &str, texts: &[&str]) -> Vec<f32> {
        self.inner.log_probabilities(context, texts)
    }
}

pub mod settle {
    /// 停顿结果能否落地:输入没变、还在一页的首行、组合仍在。
    pub fn result_is_current(
        engine_revision: u64,
        result_revision: u64,
        page: usize,
        highlight: usize,
        composing: bool,
    ) -> bool {
        engine_revision == result_revision && page == 0 && highlight == 0 && composing
    }
}
