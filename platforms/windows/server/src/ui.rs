//! 界面入口:候选窗、模式徽标与宿主选择面板。
//!
//! 候选窗在 D2D 管线里(`ui::d2d`:独立线程 + DirectComposition + Direct2D),模式徽标在原生分层窗口里(`ui::hud`:Direct2D +
//! UpdateLayeredWindow),扩展 `panel.show` 的选择面板是可获焦的原生窗口(`ui::picker`:
//! 用户挑选后归还焦点)。三条路各有自己的线程与消息泵,主进程不跑
//! UI 事件循环,窗口出问题时也不牵动输入法本身。

pub(crate) mod d2d;
mod hud;
pub(crate) mod picker;
mod place;
mod toolbar;

use std::sync::Arc;

use crate::imp::State;

/// 起候选窗(D2D 线程)、模式徽标(原生窗口线程)与选择面板(可获焦窗口线程)。
pub(super) fn run(state: Arc<State>) {
    // 选择面板:空闲时阻塞在通道上,扩展 `panel.show` 来请求才建窗;先于脚本线程装填通道。
    crate::ui::picker::spawn(Arc::clone(&state));
    // 候选窗:独立线程自建消息泵。
    crate::ui::d2d::spawn(Arc::clone(&state));
    // 模式徽标:独立线程,GPU 不可用时照样显示。
    crate::ui::hud::spawn(Arc::clone(&state));
    // 工具栏:右下角可拖动的一排按钮,独立线程。
    crate::ui::toolbar::spawn(Arc::clone(&state));
}
