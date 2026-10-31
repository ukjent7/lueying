//! 中文态英文混输模块:非拼音串给出英文词候选,保留专名大小写。
//!
//! 只补一个候选,不改中文排序。英文不是用户词:不学习、不造词,也不参与
//! 模型重排。半截拼音已有中文候选时不出原文,避免 `nih` 把内联显示顶成字母。

use std::collections::HashMap;
use std::sync::OnceLock;

use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};

/// 内置英文混输模块。
#[derive(Default)]
pub struct EnglishModule;

impl Module for EnglishModule {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "english",
            name: "英文混输",
            version: "1.0",
            capabilities: &[Capability::Candidates],
        }
    }

    fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        // 快捷触发串(v/u 前缀等)由快捷模块接管,英文不掺和。
        if ctx.quick_trigger {
            return;
        }
        let raw = ctx.raw;
        if raw.len() < 2 || !raw.bytes().all(|b| b.is_ascii_lowercase()) {
            return;
        }
        // 模糊纠正出的整句也算 full,但它不是用户要打中文的证据。`hello`
        // 这种完整英文词只在有可信整词时才让位,否则会被「和你你」压到页尾。
        let has_full = cands
            .iter()
            .any(|cand| cand.full && !cand.quick && !cand.no_learn);
        let trusted_full = cands
            .iter()
            .any(|cand| cand.full && cand.trusted && !cand.quick && !cand.no_learn);
        let text = match table().get(raw) {
            Some(text) => text.to_string(),
            None if has_full || !cands.is_empty() => return,
            None => raw.to_string(),
        };
        let mut cand = Candidate::whole(raw.len(), text, 0.0);
        cand.words.clear();
        cand.full = false;
        cand.hint = Some("英文".to_string());
        cand.no_learn = true;
        if trusted_full {
            cands.push(cand);
        } else {
            cands.insert(0, cand);
        }
    }
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../data/en.tsv")
            .lines()
            .filter_map(|line| {
                let (key, word) = line.split_once('\t')?;
                (!key.is_empty() && !word.is_empty()).then_some((key, word))
            })
            .collect()
    })
}
