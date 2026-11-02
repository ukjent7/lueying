//! 进程入口:注册 IMKServer,macOS 输入法由系统按需拉起。

use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSBundle, NSString};
use objc2_input_method_kit::IMKServer;

/// Info.plist 的 `InputMethodConnectionName` 必须与此一致。
pub const CONNECTION_NAME: &str = "Lueying_Connection";
const FALLBACK_BUNDLE_ID: &str = "com.lueying.inputmethod";

pub fn run() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("IMKServer 必须在 macOS 主线程启动");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // 输入法常驻后台,不在 Dock 里占一个图标。
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    crate::controller::register();

    let bundle = NSBundle::mainBundle();
    let identifier = bundle
        .bundleIdentifier()
        .map(|value| value.to_string())
        .unwrap_or_else(|| FALLBACK_BUNDLE_ID.to_string());
    let name = NSString::from_str(CONNECTION_NAME);
    let bundle_id = NSString::from_str(&identifier);
    let Some(server) = (unsafe {
        IMKServer::initWithName_bundleIdentifier(IMKServer::alloc(), Some(&name), Some(&bundle_id))
    }) else {
        eprintln!("IMKServer 注册失败:连接名或 Info.plist 无效");
        return;
    };
    // server 在 app.run() 期间一直存活;返回值不留,进程退出时一起结束。
    let _server = server;
    app.run();
}
