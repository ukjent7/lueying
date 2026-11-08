//! 扩展 E2E 公共件:临时扩展包与组件测试客人。
//!
//! guest 是真实 SDK 编出的组件(fixture-guest / fixture-init-loop,CI 打包),
//! 通过环境变量 `LUEYING_FIXTURE_WASM` / `LUEYING_FIXTURE_B_WASM` /
//! `LUEYING_FIXTURE_INIT_LOOP_WASM` 提供;本机没有产物时相关检查跳过。

use std::path::Path;

use crate::harness::push;
use lueying_extwasm::Pack;

/// 测试客人 A 的主题色。
pub(crate) const ACCENT_A: u32 = 0x10_20_30;
/// 测试客人 B 的主题色(变体 B)。
pub(crate) const ACCENT_B: u32 = 0x20_40_60;

/// 外观包清单。
pub(crate) fn appearance_manifest(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":4,"wasm":"extension.wasm","appearance":true}}"#
    )
}

/// 命令包清单;`needs` 与命令由调用方拼。
pub(crate) fn command_manifest(id: &str, commands: &[&str], needs: &[&str]) -> String {
    let list = commands
        .iter()
        .map(|command| format!(r#"{{"id":"{command}","title":"{command}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let services = needs
        .iter()
        .map(|service| format!(r#""{service}""#))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":4,"wasm":"extension.wasm","activate":{{"commands":[{list}]}},"needs":[{services}]}}"#
    )
}

/// 自定义清单原文;`extra` 拼在 `wasm` 字段后面。
pub(crate) fn raw_manifest(id: &str, extra: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":4,"wasm":"extension.wasm",{extra}}}"#
    )
}

/// 最小合法清单:纯模块名,不带任何可选字段。
pub(crate) fn plain_manifest(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":4,"wasm":"extension.wasm"}}"#)
}

pub(crate) fn write_pack(dir: &Path, manifest: &str, wasm: impl AsRef<[u8]>) {
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(dir.join("extension.wasm"), wasm.as_ref()).expect("写 Wasm");
}

pub(crate) fn load_or_fail(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    dir: &Path,
) -> Option<Pack> {
    match Pack::load(dir) {
        Ok(pack) => Some(pack),
        Err(e) => {
            push(checks, name, false, e);
            None
        }
    }
}

/// 读环境变量指向的组件产物;缺失或读不到就返回 `None`。
fn component_bytes(var: &str) -> Option<Vec<u8>> {
    let path = std::env::var(var).ok()?;
    std::fs::read(path).ok()
}

/// 测试客人 A:命令 `run`/`echo` 回显参数,`trap`/`spin`/`hog`/`notify` 各司其职。
pub(crate) fn fixture_a() -> Option<Vec<u8>> {
    component_bytes("LUEYING_FIXTURE_WASM")
}

/// 测试客人 B(变体):回显带 `[b]` 前缀,主题色不同。
pub(crate) fn fixture_b() -> Option<Vec<u8>> {
    component_bytes("LUEYING_FIXTURE_B_WASM")
}

/// init 死循环客人:初始化燃料打断测试用。
pub(crate) fn fixture_init_loop() -> Option<Vec<u8>> {
    component_bytes("LUEYING_FIXTURE_INIT_LOOP_WASM")
}

/// 需要测试客人的检查:拿到产物或跳过(说明打到 stderr,e2e 手动跑时可见)。
/// `None` 分支返回 `Default::default()`:辅助函数是 `()`,run 是失败数 0。
macro_rules! fixture_or_skip {
    ($loader:expr) => {
        match $loader {
            Some(bytes) => bytes,
            None => {
                eprintln!("扩展 E2E 跳过一项:没有组件测试客人产物");
                return ::std::default::Default::default();
            }
        }
    };
}

pub(crate) use fixture_or_skip;
