//! 界面入口:候选窗、模式徽标与命令面板。
//!
//! 候选窗在 D2D 管线里(`ui::d2d`:独立线程 + DirectComposition + Direct2D),模式徽标在原生分层窗口里(`ui::hud`:Direct2D +
//! UpdateLayeredWindow),命令面板是可获焦的原生窗口(`ui::panel`:
//! 总热键唤出、关掉归还焦点)。三条路各有自己的线程与消息泵,主进程不跑
//! UI 事件循环,窗口出问题时也不牵动输入法本身。

pub(crate) mod d2d;
mod hud;
pub(crate) mod panel;
mod place;

use std::sync::Arc;

use crate::imp::State;

/// 起候选窗(D2D 线程)、模式徽标(原生窗口线程)与命令面板(可获焦窗口线程)。
pub(super) fn run(state: Arc<State>) {
    // 命令面板:空闲时阻塞在通道上,总热键唤出才建窗;先于脚本线程装填通道。
    crate::ui::panel::spawn(Arc::clone(&state));
    // 候选窗:独立线程自建消息泵。
    crate::ui::d2d::spawn(Arc::clone(&state));
    // 模式徽标:独立线程,GPU 不可用时照样显示。
    crate::ui::hud::spawn(Arc::clone(&state));
}
