//! 用户词库 E2E:词记忆的学习、误选与跳过、衰减、导入导出、跨会话共享、学习键形态与私密字段。
//! 检查的细节见 `user_dict/memory.rs`。

use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Event, SharedUser};

use crate::harness::{Checks, push};

fn engine(dict: &Arc<Dictionary>, user: &SharedUser) -> Engine {
    Engine::new(Arc::clone(dict), Config::default())
        .with_user(Arc::clone(user))
        .with_modules(crate::builtins())
}

/// 翻到候选所在页并点选;用于重复学习同一个词。
fn pick_word(engine: &mut Engine, word: &str) -> bool {
    let Some(index) = engine.candidates().iter().position(|c| c == word) else {
        return false;
    };
    let page_size = engine.page_size().max(1);
    let page = index / page_size;
    while engine.page() < page {
        if !engine.page_by(1) {
            return false;
        }
    }
    engine
        .pick_in_page(index % page_size)
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text.as_str() == word))
}

mod memory;

pub fn run(report: &mut String) -> usize {
    let dict = Arc::new(Dictionary::builtin());
    let mut checks = Checks::new();
    memory::shared_sessions(&dict, &mut checks);
    memory::two_picks_promote(&dict, &mut checks);
    memory::mispick_demotes(&dict, &mut checks);
    memory::skip_weakens(&dict, &mut checks);
    memory::round_trip_and_bad_lines(&dict, &mut checks);
    memory::decay_keeps_words(&dict, &mut checks);
    memory::learning_keys(&dict, &mut checks);
    memory::typo_gets_no_bonus(&dict, &mut checks);
    memory::abbrev_inside_syllable_no_bonus(&dict, &mut checks);
    memory::abbrev_prefix_reuse_matches_fresh(&dict, &mut checks);
    memory::mutual_key_needs_dictionary(&dict, &mut checks);
    memory::raw_input_memory(&dict, &mut checks);
    memory::lone_letter_no_cross_bonus(&dict, &mut checks);

    crate::harness::write(report, "用户词库", checks)
}
