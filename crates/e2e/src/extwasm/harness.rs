//! 扩展 E2E 公共件:临时扩展包与最小 Wasm guest。
//!
//! guest 全部用 WAT 手写:不依赖 wasm32 工具链,Linux CI 也能跑全套。
//! guest 走标准 Extism ABI:从 `extism:host/env` 导入内核函数,导出按
//! 命令 id 命名的入口。

use std::path::Path;

use crate::harness::push;
use lueying_extwasm::Pack;

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

/// extism 内核函数的导入。
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

/// 分配一块内存并写进文本,返回偏移。
fn block(local: &str, text: &str) -> String {
    format!(
        "    (local.set {local} (call $alloc (i64.const {})))\n{}",
        text.len(),
        store_bytes(local, text.as_bytes())
    )
}

/// 命令返回一段固定文本。
pub(crate) fn command_guest(result: &str) -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (local $ptr i64)
{}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))",
        preamble(),
        block("$ptr", result),
        len = result.len(),
    )
}

/// 外观返回一段固定补丁。
pub(crate) fn appearance_guest(patch: &str) -> String {
    format!(
        "{}(func (export \"lueying_appearance\") (result i32)
    (local $ptr i64)
{}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))",
        preamble(),
        block("$ptr", patch),
        len = patch.len(),
    )
}

/// 命令与外观都回传错误文本;两种清单都能加载,专测错误路径。
pub(crate) fn error_guest(error: &str) -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (local $ptr i64)
{}    (call $error_set (local.get $ptr))
    (i32.const -1))
  (func (export \"lueying_appearance\") (result i32)
    (local $ptr i64)
{}    (call $error_set (local.get $ptr))
    (i32.const -1)))",
        preamble(),
        block("$ptr", error),
        block("$ptr", error),
    )
}

/// 命令与外观都直接 trap;两种清单都能加载。
pub(crate) fn trap_guest() -> String {
    format!(
        "{}(func (export \"run\") (result i32)
    (unreachable))
  (func (export \"lueying_appearance\") (result i32)
    (unreachable)))",
        preamble(),
    )
}

/// 外观第一次派发 trap,之后返回固定补丁;验证 trap 后宿主仍然可用。
pub(crate) fn once_trap_appearance_guest(patch: &str) -> String {
    format!(
        "{}(global $calls (mut i32) (i32.const 0))
  (func (export \"lueying_appearance\") (result i32)
    (local $ptr i64)
    (if (i32.eqz (global.get $calls))
      (then
        (global.set $calls (i32.const 1))
        (unreachable)))
{}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))",
        preamble(),
        block("$ptr", patch),
        len = patch.len(),
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

/// 命令不停申请内存;线性内存限额一到就 trap。
pub(crate) fn hog_guest() -> String {
    format!(
        "{}(memory 1)
  (func (export \"run\") (result i32)
    (loop $grow
      (drop (memory.grow (i32.const 16)))
      (br $grow))
    (i32.const 0)))",
        preamble(),
    )
}

/// 初始化死循环;初始化燃料烧完才停。
pub(crate) fn init_loop_guest() -> String {
    format!(
        "{}(func (export \"lueying_appearance\") (result i32)
    (i32.const 0))
  (func (export \"lueying_init\") (result i32)
    (local $x i32)
    (loop $forever
      (local.set $x (i32.add (local.get $x) (i32.const 1)))
      (br $forever))
    (i32.const 0)))",
        preamble(),
    )
}

/// 命令先睡 `ms` 毫秒再回文本;测宿主可取消等待。
pub(crate) fn sleep_guest(ms: u64) -> String {
    format!(
        "{}(import \"extism:host/user\" \"lueying_timeout\" (func $timeout (param i64) (result i32)))
  (func (export \"run\") (result i32)
    (local $code i32)
    (local $ptr i64)
    (local.set $code (call $timeout (i64.const {ms})))
    (if (i32.ne (local.get $code) (i32.const 0))
      (then
{}    (call $error_set (local.get $ptr))
        (return (i32.const -1))))
{}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))",
        preamble(),
        block("$ptr", "睡被取消"),
        block("$ptr", r#"{"slept":true}"#),
        len = r#"{"slept":true}"#.len(),
    )
}

/// 命令调用一个宿主服务并把结果原样交回;方法与参数固定。
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
{}    (local.set $resp (call $svc (local.get $req)))
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
        block("$req", &request),
    )
}
