//! 界面入口:候选窗与模式徽标。
//!
//! 候选窗在 Skia 管线里(`ui_skia`:独立线程 + DirectComposition +
//! Ganesh-D3D12),模式徽标在原生分层窗口里(`ui_hud`:CPU 光栅 +
//! UpdateLayeredWindow)。两条路各有自己的线程与消息泵,主进程不跑 UI
//! 事件循环,窗口出问题时也不牵动输入法本身。

use std::sync::Arc;

use crate::imp::State;

/// 起候选窗(Skia 线程)与模式徽标(原生窗口线程)。
pub(super) fn run(state: Arc<State>) {
    // 候选窗:独立线程自建消息泵。
    crate::ui_skia::spawn(Arc::clone(&state));
    // 模式徽标:独立线程,GPU 不可用时照样显示。
    crate::ui_hud::spawn(Arc::clone(&state));
}
