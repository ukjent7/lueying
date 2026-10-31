//! 设置页的无障碍树:把 `settings_view` 的稳定 ID 映射成 AccessKit 节点。
//!
//! 窗口线程每次重排后重建整棵树(几十个节点,代价可以忽略),快照交给
//! `SubclassingAdapter`;读屏软件请求初始树时直接返回快照。动作处理器只
//! 把 `NodeId` 与动作放进队列,由窗口线程翻译回 `Id` 后走和鼠标点击同一条
//! `Window::press`,自绘控件与原生按钮的行为保持一致。

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Rect, Role, Toggled,
    TreeId, TreeInfo, TreeUpdate,
};
use accesskit_windows::SubclassingAdapter;
use lueying_render::ui::{Button, Control, Id, Layout, Page};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::settings_view::Settings;

/// 无障碍动作队列有新条目时投给窗口的消息。
pub const WM_A11Y: u32 = WM_APP + 2;

/// 读屏软件发来的、已经翻译成窗口内部动作的请求。
#[derive(Clone, Copy, Debug)]
pub enum QueuedAction {
    Click(NodeId),
    Focus(NodeId),
}

/// 窗口持有的适配器;共享树与动作队列同时给激活/动作处理器。
pub struct A11y {
    adapter: SubclassingAdapter,
    tree: Arc<Mutex<TreeUpdate>>,
}

impl A11y {
    /// 挂上适配器。必须在窗口首次显示或获焦之前调用窗口线程上。
    pub fn new(
        hwnd: HWND,
        tree: Arc<Mutex<TreeUpdate>>,
        actions: Arc<Mutex<Vec<QueuedAction>>>,
    ) -> Self {
        let activation = Activation {
            tree: Arc::clone(&tree),
        };
        let handler = Actions {
            hwnd: hwnd.0 as usize,
            queue: Arc::clone(&actions),
        };
        Self {
            adapter: SubclassingAdapter::new(hwnd, activation, handler),
            tree,
        }
    }

    /// 树已更新:只在适配器激活时下发,并把事件同步抛出去。
    pub fn refresh(&mut self) {
        let tree = Arc::clone(&self.tree);
        let update = self.adapter.update_if_active(move || {
            tree.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        });
        if let Some(events) = update {
            events.raise();
        }
    }
}

struct Activation {
    tree: Arc<Mutex<TreeUpdate>>,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(
            self.tree
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
        )
    }
}

struct Actions {
    hwnd: usize,
    queue: Arc<Mutex<Vec<QueuedAction>>>,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        let queued = match &request.action {
            Action::Click => QueuedAction::Click(request.target_node),
            Action::Focus => QueuedAction::Focus(request.target_node),
            _ => return,
        };
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(queued);
        // 处理器可能跑在别的线程,只把唤醒消息投回窗口线程。
        let hwnd = HWND(self.hwnd as *mut c_void);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_A11Y, WPARAM(0), LPARAM(0));
        }
    }
}

/// 一棵树加一张 `NodeId → 控件 ID` 的反查表。
pub struct Snapshot {
    pub update: TreeUpdate,
    pub ids: Vec<(NodeId, Id)>,
}

impl Snapshot {
    /// 动作目标反查控件;只包含可交互的控件。
    pub fn control(&self, node: NodeId) -> Option<Id> {
        self.ids
            .iter()
            .find_map(|(id, control)| (*id == node).then_some(*control))
    }
}

/// 从当前页面与几何生成整棵树。`scroll` 是卡片往上滚过的物理像素。
pub fn page_tree(
    page: &Page,
    layout: &Layout,
    settings: &Settings,
    focus: Option<Id>,
    scroll: f32,
    size: (i32, i32),
) -> Snapshot {
    let mut nodes: Vec<(NodeId, Node)> = Vec::new();
    let mut ids: Vec<(NodeId, Id)> = Vec::new();
    let mut children: Vec<NodeId> = Vec::new();

    children.push(text_node(
        "@title",
        0,
        Role::Label,
        page.title.clone(),
        None,
        &mut nodes,
    ));
    children.push(text_node(
        "@subtitle",
        0,
        Role::Label,
        page.subtitle.clone(),
        None,
        &mut nodes,
    ));
    if let Some(button) = &page.header_button {
        let node = node_id(button.id.base, button.id.index);
        nodes.push((
            node,
            button_node(button, "版本更新", page_bounds(layout, button.id)),
        ));
        ids.push((node, button.id));
        children.push(node);
    }

    for (group_index, group) in page.groups.iter().enumerate() {
        let group_id = node_id("@group", group_index);
        let mut group_children: Vec<NodeId> = Vec::new();
        for (row_index, row) in group.rows.iter().enumerate() {
            let row_key = group_index * 100 + row_index;
            match &row.control {
                Control::Toggle { id, on } => {
                    let node = node_id(id.base, id.index);
                    let mut toggle = Node::new(Role::Switch);
                    toggle.set_label(row.label.clone());
                    toggle.set_description(row.detail.clone());
                    toggle.set_toggled(toggled(*on));
                    toggle.add_action(Action::Click);
                    toggle.add_action(Action::Focus);
                    if let Some(bounds) = card_bounds(layout, *id, scroll) {
                        toggle.set_bounds(bounds);
                    }
                    nodes.push((node, toggle));
                    ids.push((node, *id));
                    group_children.push(node);
                }
                Control::Segment {
                    id,
                    options,
                    selected,
                    ..
                } => {
                    let node = node_id("@segment", row_key);
                    let mut segment = Node::new(Role::RadioGroup);
                    segment.set_label(row.label.clone());
                    segment.set_description(row.detail.clone());
                    if let Some(bounds) = segment_bounds(layout, *id, scroll) {
                        segment.set_bounds(bounds);
                    }
                    let mut cells = Vec::new();
                    for (index, option) in options.iter().enumerate() {
                        let control = Id::new(id.base, index);
                        let cell_id = node_id(control.base, control.index);
                        let mut cell = Node::new(Role::RadioButton);
                        cell.set_label(option.text.clone());
                        cell.set_selected(index == *selected);
                        cell.set_position_in_set(index);
                        cell.set_size_of_set(options.len());
                        cell.add_action(Action::Click);
                        cell.add_action(Action::Focus);
                        if let Some(bounds) = card_bounds(layout, control, scroll) {
                            cell.set_bounds(bounds);
                        }
                        nodes.push((cell_id, cell));
                        ids.push((cell_id, control));
                        cells.push(cell_id);
                    }
                    segment.set_children(cells);
                    nodes.push((node, segment));
                    group_children.push(node);
                }
                Control::Buttons { items } => {
                    let node = node_id("@buttons", row_key);
                    let mut buttons = Node::new(Role::Group);
                    buttons.set_label(row.label.clone());
                    buttons.set_description(row.detail.clone());
                    let mut items_nodes = Vec::new();
                    for button in items {
                        let control = node_id(button.id.base, button.id.index);
                        nodes.push((
                            control,
                            button_node(button, &row.label, card_bounds(layout, button.id, scroll)),
                        ));
                        ids.push((control, button.id));
                        items_nodes.push(control);
                    }
                    buttons.set_children(items_nodes);
                    nodes.push((node, buttons));
                    group_children.push(node);
                }
                Control::ButtonsToggle {
                    items,
                    toggle,
                    toggle_on,
                } => {
                    let node = node_id("@buttons", row_key);
                    let mut group = Node::new(Role::Group);
                    group.set_label(row.label.clone());
                    group.set_description(row.detail.clone());
                    let mut items_nodes = Vec::new();
                    for button in items {
                        let control = node_id(button.id.base, button.id.index);
                        nodes.push((
                            control,
                            button_node(button, &row.label, card_bounds(layout, button.id, scroll)),
                        ));
                        ids.push((control, button.id));
                        items_nodes.push(control);
                    }
                    let switch_id = node_id(toggle.base, toggle.index);
                    let mut switch = Node::new(Role::Switch);
                    switch.set_label(row.label.clone());
                    switch.set_description(row.detail.clone());
                    switch.set_toggled(toggled(*toggle_on));
                    switch.add_action(Action::Click);
                    switch.add_action(Action::Focus);
                    if let Some(bounds) = card_bounds(layout, *toggle, scroll) {
                        switch.set_bounds(bounds);
                    }
                    nodes.push((switch_id, switch));
                    ids.push((switch_id, *toggle));
                    items_nodes.push(switch_id);
                    group.set_children(items_nodes);
                    nodes.push((node, group));
                    group_children.push(node);
                }
                Control::None => {
                    group_children.push(text_node(
                        "@label",
                        row_key,
                        Role::Label,
                        row.label.clone(),
                        Some(row.detail.clone()),
                        &mut nodes,
                    ));
                }
            }
        }
        let mut group_node = Node::new(Role::Group);
        group_node.set_label(group.title.clone());
        group_node.set_children(group_children);
        nodes.push((group_id, group_node));
        children.push(group_id);
    }

    children.push(text_node(
        "@notice",
        0,
        Role::Status,
        page.notice.clone(),
        None,
        &mut nodes,
    ));
    for button in &page.footer_buttons {
        let node = node_id(button.id.base, button.id.index);
        nodes.push((
            node,
            button_node(button, "设置窗", page_bounds(layout, button.id)),
        ));
        ids.push((node, button.id));
        children.push(node);
    }

    let root = node_id("@root", 0);
    let mut window = Node::new(Role::Window);
    window.set_label("掠影设置");
    window.set_description(page.subtitle.clone());
    window.set_bounds(Rect::new(
        0.0,
        0.0,
        f64::from(size.0.max(0)),
        f64::from(size.1.max(0)),
    ));
    window.set_children(children);
    nodes.push((root, window));

    let focus = focus.map_or(root, |id| {
        let index = if Settings::is_segment(id.base) {
            settings.selected(id.base)
        } else {
            id.index
        };
        node_id(id.base, index)
    });
    let focus = if focus == root || nodes.iter().any(|(node, _)| *node == focus) {
        focus
    } else {
        root
    };
    Snapshot {
        update: TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(root)),
            tree_id: TreeId::ROOT,
            focus,
        },
        ids,
    }
}

fn text_node(
    base: &str,
    index: usize,
    role: Role,
    label: String,
    detail: Option<String>,
    nodes: &mut Vec<(NodeId, Node)>,
) -> NodeId {
    let node = node_id(base, index);
    let mut item = Node::new(role);
    // Label 的 UIA 名称来自 value 属性;其余角色用 label。
    if role == Role::Label {
        item.set_value(label);
    } else {
        item.set_label(label);
    }
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        item.set_description(detail);
    }
    nodes.push((node, item));
    node
}

fn button_node(button: &Button, description: &str, bounds: Option<Rect>) -> Node {
    let mut node = Node::new(Role::Button);
    node.set_label(button.text.clone());
    if !description.is_empty() {
        node.set_description(description.to_string());
    }
    if button.enabled {
        node.add_action(Action::Click);
        node.add_action(Action::Focus);
    } else {
        node.set_disabled();
    }
    if let Some(bounds) = bounds {
        node.set_bounds(bounds);
    }
    node
}

fn toggled(on: bool) -> Toggled {
    if on { Toggled::True } else { Toggled::False }
}

fn page_bounds(layout: &Layout, id: Id) -> Option<Rect> {
    layout.page_hit(id).map(page_rect)
}

fn card_bounds(layout: &Layout, id: Id, scroll: f32) -> Option<Rect> {
    layout
        .card_hit(id)
        .map(|rect| card_rect(layout, rect, scroll))
}

fn segment_bounds(layout: &Layout, id: Id, scroll: f32) -> Option<Rect> {
    layout
        .ring_card(id)
        .map(|rect| card_rect(layout, rect, scroll))
}

fn page_rect(rect: skia_safe::Rect) -> Rect {
    Rect::new(
        f64::from(rect.left),
        f64::from(rect.top),
        f64::from(rect.right),
        f64::from(rect.bottom),
    )
}

fn card_rect(layout: &Layout, rect: skia_safe::Rect, scroll: f32) -> Rect {
    Rect::new(
        f64::from(layout.card.left + rect.left),
        f64::from(layout.card.top + rect.top - scroll),
        f64::from(layout.card.left + rect.right),
        f64::from(layout.card.top + rect.bottom - scroll),
    )
}

/// 控件 ID 到节点 ID:FNV-1a,跨更新稳定;同一控件每次都映射到同一节点。
fn node_id(base: &str, index: usize) -> NodeId {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in base.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= index as u64;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    NodeId::from(hash)
}

#[cfg(test)]
mod tests {
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
        assert_eq!(snapshot.control(node), Some(id));
        assert_eq!(snapshot.control(node_id("aux", 4)), None);
    }
}
