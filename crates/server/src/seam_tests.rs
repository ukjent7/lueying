//! 缝的边界:槽位的失败停用、结论校验与旁路应答解析。

use std::sync::Arc;

use super::{SeamOutcome, SettledRerank, SettledSlot};

/// 永远给同一个下标的提供者。
struct Always(Option<usize>);

impl SettledRerank for Always {
    fn name(&self) -> &str {
        "正常"
    }
    fn promote(&self, _context: &str, _texts: &[&str]) -> Result<Option<usize>, String> {
        Ok(self.0)
    }
}

/// 永远报错的提供者。
struct Faulty;

impl SettledRerank for Faulty {
    fn name(&self) -> &str {
        "坏提供者"
    }
    fn promote(&self, _context: &str, _texts: &[&str]) -> Result<Option<usize>, String> {
        Err("模型没起来".into())
    }
}

const TEXTS: [&str; 2] = ["你好", "你号"];

#[test]
fn empty_slot_reports_builtin() {
    let mut slot = SettledSlot::new();
    assert!(!slot.has_provider());
    assert!(matches!(slot.promote("前文", &TEXTS), SeamOutcome::Builtin));
}

#[test]
fn provider_index_passes_through() {
    let mut slot = SettledSlot::new();
    slot.set(Some(Arc::new(Always(Some(1)))));
    assert!(slot.has_provider());
    assert!(matches!(
        slot.promote("前文", &TEXTS),
        SeamOutcome::Provider(Some(1))
    ));
    slot.set(Some(Arc::new(Always(None))));
    assert!(matches!(
        slot.promote("前文", &TEXTS),
        SeamOutcome::Provider(None)
    ));
}

#[test]
fn out_of_range_index_counts_as_fault() {
    let mut slot = SettledSlot::new();
    slot.set(Some(Arc::new(Always(Some(9)))));
    assert!(matches!(slot.promote("前文", &TEXTS), SeamOutcome::Builtin));
    // 越界是失败,但一次不算停用;正常结论把计数清回零。
    slot.set(Some(Arc::new(Always(Some(1)))));
    assert!(matches!(
        slot.promote("前文", &TEXTS),
        SeamOutcome::Provider(Some(1))
    ));
}

#[test]
fn three_consecutive_faults_disable_the_provider() {
    let mut slot = SettledSlot::new();
    slot.set(Some(Arc::new(Faulty)));
    for _ in 0..(super::MAX_FAULTS - 1) {
        assert!(matches!(slot.promote("前文", &TEXTS), SeamOutcome::Builtin));
        assert!(slot.has_provider(), "没到阈值不停用");
    }
    assert!(matches!(slot.promote("前文", &TEXTS), SeamOutcome::Builtin));
    assert!(!slot.has_provider(), "第三次失败即停用");
    // 停用之后槽位回到纯内置,不再问提供者。
    assert!(matches!(slot.promote("前文", &TEXTS), SeamOutcome::Builtin));
}

#[test]
fn replacing_the_provider_resets_faults() {
    let mut slot = SettledSlot::new();
    slot.set(Some(Arc::new(Faulty)));
    assert!(matches!(slot.promote("前文", &TEXTS), SeamOutcome::Builtin));
    slot.set(Some(Arc::new(Always(Some(0)))));
    assert!(matches!(
        slot.promote("前文", &TEXTS),
        SeamOutcome::Provider(Some(0))
    ));
}

#[test]
fn bypass_promote_parse() {
    assert_eq!(
        super::parse_promote(r#"{"result":{"index":1}}"#),
        Ok(Some(1))
    );
    assert_eq!(
        super::parse_promote(r#"{"result":{"index":null}}"#),
        Ok(None)
    );
    assert_eq!(
        super::parse_promote(r#"{"error":"模型加载失败"}"#),
        Err("模型加载失败".into())
    );
    assert!(super::parse_promote("不是 JSON").is_err());
    assert!(
        super::parse_promote(r#"{"result":{}}"#).is_err(),
        "缺 index"
    );
    assert!(super::parse_promote(r#"{"result":{"index":"x"}}"#).is_err());
}
