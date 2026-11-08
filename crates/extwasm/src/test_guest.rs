//! 测试用的临时包目录、清单与组件测试客人加载。
//!
//! 行为客人是一个真实 SDK 编出的组件(`tests/fixture-guest`,CI 打包),
//! 命令与外观覆盖错误、trap、燃料与宿主服务路径。环境变量
//! `LUEYING_FIXTURE_WASM` / `LUEYING_FIXTURE_B_WASM` 指向打包好的组件;
//! 本机没有产物时行为测试跳过(CI 是门禁)。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

/// 测试客人外观补丁的固定主题色(fixture A)。
pub(crate) const FIXTURE_ACCENT: u32 = 0x10_20_30;
/// 变体 B 外观补丁的主题色;热重载与合并顺序用它与 A 区分。
pub(crate) const FIXTURE_B_ACCENT: u32 = 0x20_40_60;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// 一个临时目录;测试结束自动删除。
pub(crate) struct Temp(PathBuf);

impl Temp {
    pub(crate) fn new(name: &str) -> Self {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lueying-extwasm-{}-{}-{name}",
            std::process::id(),
            unique
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self(path)
    }

    pub(crate) fn pack(&self, id: &str) -> PathBuf {
        self.0.join(id)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 写一个包目录:清单 + `extension.wasm`。
pub(crate) fn write_pack(dir: &Path, manifest: &str, wasm: impl AsRef<[u8]>) {
    std::fs::create_dir_all(dir).expect("建包目录");
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(dir.join("extension.wasm"), wasm.as_ref()).expect("写 Wasm");
}

/// 一份带命令的清单。
pub(crate) fn command_manifest(id: &str, commands: &[(&str, &str)]) -> String {
    json!({
        "id": id,
        "name": "演示",
        "version": "1.0.0",
        "api": 4,
        "wasm": "extension.wasm",
        "activate": {
            "commands": commands
                .iter()
                .map(|(id, title)| json!({ "id": id, "title": title }))
                .collect::<Vec<_>>()
        }
    })
    .to_string()
}

/// 一份只提供外观的清单。
pub(crate) fn appearance_manifest(id: &str) -> String {
    json!({
        "id": id,
        "name": "演示",
        "version": "1.0.0",
        "api": 4,
        "wasm": "extension.wasm",
        "appearance": true
    })
    .to_string()
}

/// 一份带命令与宿主服务声明的清单。
pub(crate) fn command_manifest_with_needs(
    id: &str,
    commands: &[(&str, &str)],
    needs: &[&str],
) -> String {
    json!({
        "id": id,
        "name": "演示",
        "version": "1.0.0",
        "api": 4,
        "wasm": "extension.wasm",
        "activate": {
            "commands": commands
                .iter()
                .map(|(id, title)| json!({ "id": id, "title": title }))
                .collect::<Vec<_>>()
        },
        "needs": needs
    })
    .to_string()
}

/// 一份没有任何入口的清单。
pub(crate) fn plain_manifest(id: &str) -> String {
    json!({
        "id": id,
        "name": "演示",
        "version": "1.0.0",
        "api": 4,
        "wasm": "extension.wasm"
    })
    .to_string()
}

/// 读环境变量指向的组件产物;缺失或读不到就返回 `None`(跳过)。
fn component_bytes(var: &str) -> Option<Vec<u8>> {
    let path = std::env::var(var).ok()?;
    std::fs::read(path).ok()
}

/// 测试客人(变体 A):命令 `run`/`echo` 回显参数。
pub(crate) fn fixture() -> Option<Vec<u8>> {
    component_bytes("LUEYING_FIXTURE_WASM")
}

/// 测试客人变体 B:回显带 `[b]` 前缀,热重载与合并顺序用。
pub(crate) fn fixture_b() -> Option<Vec<u8>> {
    component_bytes("LUEYING_FIXTURE_B_WASM")
}

/// 需要测试客人的测试:拿到产物或跳过。
/// `None` 分支返回 `Default::default()`:测试函数是 `()`,e2e 的 run 是失败数 0。
macro_rules! fixture_or_skip {
    ($loader:expr, $what:literal) => {
        match $loader {
            Some(bytes) => bytes,
            None => {
                eprintln!("跳过:{}(没有组件测试客人产物)", $what);
                return ::std::default::Default::default();
            }
        }
    };
}

pub(crate) use fixture_or_skip;
