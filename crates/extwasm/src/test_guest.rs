//! 测试用的最小 Wasm guest 与临时包目录。
//!
//! guest 走标准 Extism ABI:从 `extism:host/env` 导入内核函数,导出按
//! 命令 id 命名的入口。这样测的路径与第三方插件完全一致。

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

/// 公共骨架:extism 内核函数的导入。
fn preamble() -> &'static str {
    r#"(module
  (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
  (import "extism:host/env" "free" (func $free (param i64)))
  (import "extism:host/env" "length" (func $length (param i64) (result i64)))
  (import "extism:host/env" "load_u8" (func $load_u8 (param i64) (result i32)))
  (import "extism:host/env" "store_u8" (func $store_u8 (param i64 i32)))
  (import "extism:host/env" "output_set" (func $output_set (param i64 i64)))
  (import "extism:host/env" "error_set" (func $error_set (param i64)))
"#
}

/// 把字节逐个写进 extism 内存;偏移从 `local` 里的块首地址开始。
fn store_bytes(local: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        out.push_str(&format!(
            "    (call $store_u8 (i64.add (local.get {local}) (i64.const {index})) (i32.const {byte}))\n"
        ));
    }
    out
}

/// 命令返回一段固定文本。
pub(crate) fn command_guest(result: &str) -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (local $ptr i64)
    (local.set $ptr (call $alloc (i64.const {len})))
{stores}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))",
        preamble(),
        len = result.len(),
        stores = store_bytes("$ptr", result.as_bytes()),
    )
}

/// 命令回传错误文本。
pub(crate) fn error_guest(error: &str) -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (local $ptr i64)
    (local.set $ptr (call $alloc (i64.const {len})))
{stores}    (call $error_set (local.get $ptr))
    (i32.const -1)))",
        preamble(),
        len = error.len(),
        stores = store_bytes("$ptr", error.as_bytes()),
    )
}

/// 命令直接 trap。
pub(crate) fn trap_guest() -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (unreachable)
    (i32.const 0)))",
        preamble(),
    )
}

/// 命令死循环;燃料烧完才停。
pub(crate) fn loop_guest() -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (local $x i32)
    (loop $forever
      ;; 循环体放一条真实指令,保证烧燃料。
      (local.set $x (i32.add (local.get $x) (i32.const 1)))
      (br $forever))
    (i32.const 0)))",
        preamble(),
    )
}

/// 外观返回一段固定补丁。
pub(crate) fn appearance_guest(patch: &str) -> String {
    format!(
        "{}(func (export \"lueying_appearance\") (result i32)
    (local $ptr i64)
    (local.set $ptr (call $alloc (i64.const {len})))
{stores}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))",
        preamble(),
        len = patch.len(),
        stores = store_bytes("$ptr", patch.as_bytes()),
    )
}

/// 调一次宿主服务,把响应原样交回;服务失败也照样返回信封。
pub(crate) fn service_guest(service: &str, method: &str, args: &str) -> String {
    let request = format!(r#"{{"method":"{method}","args":{args}}}"#);
    format!(
        "{}(import \"extism:host/user\" \"lueying_{service}\" (func $svc (param i64) (result i64)))
  (func (export \"run\") (result i32)
    (local $req i64)
    (local $resp i64)
    (local $len i64)
    (local $i i64)
    (local $out i64)
    (local.set $req (call $alloc (i64.const {len})))
{stores}    (local.set $resp (call $svc (local.get $req)))
    (local.set $len (call $length (local.get $resp)))
    (local.set $out (call $alloc (local.get $len)))
    (local.set $i (i64.const 0))
    (block $done
      (loop $copy
        (br_if $done (i64.ge_u (local.get $i) (local.get $len)))
        (call $store_u8
          (i64.add (local.get $out) (local.get $i))
          (call $load_u8 (i64.add (local.get $resp) (local.get $i))))
        (local.set $i (i64.add (local.get $i) (i64.const 1)))
        (br $copy)))
    (call $output_set (local.get $out) (local.get $len))
    (i32.const 0)))",
        preamble(),
        len = request.len(),
        stores = store_bytes("$req", request.as_bytes()),
    )
}
