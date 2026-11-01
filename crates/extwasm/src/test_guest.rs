//! 测试用的最小 Wasm guest 与临时包目录。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

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
pub(crate) fn write_pack(dir: &Path, manifest: &str, wasm: &str) {
    std::fs::create_dir_all(dir).expect("建包目录");
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(dir.join("extension.wasm"), wasm).expect("写 Wasm");
}

/// 一份带命令的清单。
pub(crate) fn command_manifest(id: &str, commands: &[(&str, &str)]) -> String {
    json!({
        "id": id,
        "name": "演示",
        "version": "1.0.0",
        "api": 3,
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
        "api": 3,
        "wasm": "extension.wasm",
        "appearance": true
    })
    .to_string()
}

/// WAT 字符串字面量转义;长度按 UTF-8 字节算。
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// 公共骨架:内存、bump 分配器、结果缓冲区与 init。
fn preamble() -> &'static str {
    r#"(module
  (import "lueying" "emit" (func $emit (param i32 i32 i32)))
  (memory (export "memory") 2)
  (global $bump (mut i32) (i32.const 1024))
  (func (export "lueying_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $bump))
    (global.set $bump (i32.add (global.get $bump) (local.get $len)))
    (local.get $ptr))
  (func (export "lueying_free") (param i32 i32))
  (func (export "lueying_result_ptr") (result i32) (i32.const 65536))
  (func (export "lueying_result_cap") (result i32) (i32.const 4096))
  (func (export "lueying_init") (result i32) (i32.const 0))
"#
}

/// 命令返回一段固定文本。
pub(crate) fn command_guest(result: &str) -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (call $emit (i32.const 8) (i32.const {len}) (i32.const 1))
    (i32.const 0))
  (data (i32.const 8) \"{text}\"))",
        preamble(),
        len = result.len(),
        text = escape(result),
    )
}

/// 命令回传错误文本。
pub(crate) fn error_guest(error: &str) -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (call $emit (i32.const 8) (i32.const {len}) (i32.const 0))
    (i32.const 1))
  (data (i32.const 8) \"{text}\"))",
        preamble(),
        len = error.len(),
        text = escape(error),
    )
}

/// 命令直接 trap。
pub(crate) fn trap_guest() -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (unreachable)
    (i32.const 0)))",
        preamble(),
    )
}

/// 命令死循环;燃料烧完才停。
pub(crate) fn loop_guest() -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (local $x i32)
    (loop $forever
      ;; wasmtime 给 block/loop/br 记 0 燃料,循环体放一条真实指令保证烧燃料。
      (local.set $x (i32.add (local.get $x) (i32.const 1)))
      (br $forever))
    (i32.const 0)))",
        preamble(),
    )
}

/// 外观返回一段固定补丁。
pub(crate) fn appearance_guest(patch: &str) -> String {
    format!(
        "{}(func (export \"lueying_appearance\") (param i32 i32) (result i32)
    (call $emit (i32.const 8) (i32.const {len}) (i32.const 1))
    (i32.const 0))
  (data (i32.const 8) \"{text}\"))",
        preamble(),
        len = patch.len(),
        text = escape(patch),
    )
}
