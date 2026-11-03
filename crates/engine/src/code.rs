//! 形码候选:把码表命中变成引擎候选。
//!
//! 码输入(五笔、仓颉)不吃音节:一个候选吃掉整串输入,与拼音词库是两套
//! 键空间,因此不学习、不参与模型重排。

use crate::query::Built;
use crate::scheme::CodeCandidate;
use crate::Candidate;

/// 把码表命中变成候选。整串输入被一个候选吃掉。
pub(crate) fn build(codes: Vec<CodeCandidate>, raw_len: usize) -> Built {
    let cands = codes
        .into_iter()
        .map(|code| {
            let mut cand = Candidate::whole(raw_len, code.text, code.weight as f64);
            cand.no_learn = true;
            cand.full = code.full;
            cand
        })
        .collect();
    Built {
        cands,
        safe_extend: false,
        typing_prefix: false,
    }
}
