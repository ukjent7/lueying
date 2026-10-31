//! 辅助功能树与动作目标的回归测试。

use super::*;

#[test]
fn node_ids_are_stable_and_distinct() {
    assert_eq!(node_id("aux", 0), node_id("aux", 0));
    assert_ne!(node_id("aux", 0), node_id("aux", 1));
    assert_ne!(node_id("aux", 0), node_id("scheme", 0));
    assert_ne!(node_id("aux", 0), NodeId::from(0));
}

#[test]
fn snapshot_looks_up_controls() {
    let id = Id::new("aux", 3);
    let node = node_id(id.base, id.index);
    let snapshot = Snapshot {
        update: TreeUpdate {
            nodes: Vec::new(),
            tree: None,
            tree_id: TreeId::ROOT,
            focus: node,
        },
        ids: vec![(node, id)],
    };
    assert!(snapshot.ids.iter().any(|(n, c)| *n == node && *c == id));
    assert!(!snapshot.ids.iter().any(|(n, _)| *n == node_id("aux", 4)));
}
