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
