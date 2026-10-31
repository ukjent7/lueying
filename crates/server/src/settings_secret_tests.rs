use super::*;

#[test]
fn rejects_bad_ids() {
    assert!(set("../evil", "token", "x").is_err());
    assert!(set("demo", "Bad Key", "x").is_err());
}
