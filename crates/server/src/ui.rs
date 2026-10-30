//! 界面入口:候选窗、模式徽标与命令面板。
//!
//! 候选窗在 Skia 管线里(`ui_skia`:独立线程 + DirectComposition +
//! Ganesh-D3D12),模式徽标在原生分层窗口里(`ui_hud`:CPU 光栅 +
//! UpdateLayeredWindow),命令面板是可获焦的原生窗口(`ui_panel`:
//! 总热键唤出、关掉归还焦点)。三条路各有自己的线程与消息泵,主进程不跑
//! UI 事件循环,窗口出问题时也不牵动输入法本身。

use std::sync::Arc;

use crate::imp::State;

/// 起候选窗(Skia 线程)、模式徽标(原生窗口线程)与命令面板(可获焦窗口线程)。
pub(super) fn run(state: Arc<State>) {
    // 命令面板:空闲时阻塞在通道上,总热键唤出才建窗;先于脚本线程装填通道。
    crate::ui_panel::spawn(Arc::clone(&state));
    // 候选窗:独立线程自建消息泵。
    crate::ui_skia::spawn(Arc::clone(&state));
    // 模式徽标:独立线程,GPU 不可用时照样显示。
    crate::ui_hud::spawn(Arc::clone(&state));
}
