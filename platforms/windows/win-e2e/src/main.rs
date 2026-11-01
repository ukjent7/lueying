//! Windows 全链路 E2E:进程内直调 DllRegisterServer 注册 DLL → 激活本输入法 →
//! 自建 RichEdit 宿主(TSF 文本存区)→ SendInput 注入「nihao+空格」→ 读回「你好」,
//! 再切英文打 ASCII、切回中文验证全局模式。产物 target/win-e2e-report.md。

fn main() {
    #[cfg(windows)]
    {
        imp::run();
    }
}

#[cfg(windows)]
mod imp;

/// 扩展运行指标快照:服务端写入后检查 shot-ocr 的内存与派发。
#[cfg(windows)]
mod ext_metrics;

/// 设置窗无障碍:UIA 读 AccessKit 树,再打开短语编辑器验证文本输入。
#[cfg(windows)]
mod settings_a11y;

/// 扩展平台验证(任务书 V1–V8):OCR、体积、UIA 可编辑、热键,报告进 artifact。
#[cfg(windows)]
mod verify;
