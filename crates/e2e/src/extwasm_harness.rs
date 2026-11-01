//! 扩展 E2E 公共件:临时扩展包与最小 Wasm guest。
//!
//! guest 全部用 WAT 手写:不依赖 wasm32 工具链,Linux CI 也能跑全套。

use std::path::{Path, PathBuf};

use crate::harness::push;
use lueying_extwasm::Pack;

/// 外观包清单。
pub(crate) fn appearance_manifest(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":3,"wasm":"extension.wasm","appearance":true}}"#
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
        r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":3,"wasm":"extension.wasm","activate":{{"commands":[{list}]}},"needs":[{services}]}}"#
    )
}

/// 自定义清单原文;`extra` 拼在 `wasm` 字段后面。
pub(crate) fn raw_manifest(id: &str, extra: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":3,"wasm":"extension.wasm",{extra}}}"#
    )
}

/// 最小合法清单:纯模块名,不带任何可选字段。
pub(crate) fn plain_manifest(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":3,"wasm":"extension.wasm"}}"#)
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

/// 命令与外观都回传错误文本;两种清单都能加载,专测错误路径。
pub(crate) fn error_guest(error: &str) -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (call $emit (i32.const 8) (i32.const {len}) (i32.const 0))
    (i32.const 1))
  (func (export \"lueying_appearance\") (param i32 i32) (result i32)
    (call $emit (i32.const 8) (i32.const {len}) (i32.const 0))
    (i32.const 1))
  (data (i32.const 8) \"{text}\"))",
        preamble(),
        len = error.len(),
        text = escape(error),
    )
}

/// 命令与外观都直接 trap;两种清单都能加载。
pub(crate) fn trap_guest() -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (unreachable))
  (func (export \"lueying_appearance\") (param i32 i32) (result i32)
    (unreachable)))",
        preamble(),
    )
}

/// 外观第一次派发 trap,之后返回固定补丁;验证 trap 后重建仍然可用。
pub(crate) fn once_trap_appearance_guest(patch: &str) -> String {
    format!(
        "{}(global $calls (mut i32) (i32.const 0))
  (func (export \"lueying_appearance\") (param i32 i32) (result i32)
    (if (i32.eqz (global.get $calls))
      (then
        (global.set $calls (i32.const 1))
        (unreachable)))
    (call $emit (i32.const 8) (i32.const {len}) (i32.const 1))
    (i32.const 0))
  (data (i32.const 8) \"{text}\"))",
        preamble(),
        len = patch.len(),
        text = escape(patch),
    )
}

/// 命令死循环;燃料烧完才停。
pub(crate) fn loop_guest() -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (loop $forever (br $forever))
    (i32.const 0)))",
        preamble(),
    )
}

/// 命令不停申请内存;线性内存限额一到就 trap。
pub(crate) fn hog_guest() -> String {
    format!(
        "{}(func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (loop $grow
      (drop (memory.grow (i32.const 16)))
      (br $grow))
    (i32.const 0)))",
        preamble(),
    )
}

/// `lueying_init` 死循环;初始化燃料烧完才停。
pub(crate) fn init_loop_guest() -> String {
    format!(
        "{}(func (export \"lueying_appearance\") (param i32 i32) (result i32)
    (i32.const 0))
  (func (export \"lueying_init\") (result i32)
    (loop $forever (br $forever))
    (i32.const 0)))",
        preamble(),
    )
}

/// 命令先睡 `ms` 毫秒再回文本;测宿主可取消等待。
pub(crate) fn sleep_guest(ms: u64) -> String {
    format!(
        "{}(import \"lueying\" \"timeout\" (func $timeout (param i64) (result i32)))
  (func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (local $code i32)
    (local.set $code (call $timeout (i64.const {ms})))
    (if (i32.ne (local.get $code) (i32.const 0))
      (then
        (call $emit (i32.const 2048) (i32.const 12) (i32.const 0))
        (return (i32.const 1))))
    (call $emit (i32.const 8) (i32.const 13) (i32.const 1))
    (i32.const 0))
  (data (i32.const 8) \"{{\\\"slept\\\":true}}\")
  (data (i32.const 2048) \"睡被取消\"))",
        preamble(),
    )
}

/// 命令调用一个宿主服务并把结果信封转交;`method` 与 `args` 固定。
pub(crate) fn service_guest(service: &str, method: &str, args: &str) -> String {
    let method_ptr = 8 + method.len();
    let args_ptr = method_ptr + args.len();
    format!(
        "{}(import \"lueying\" \"{service}\" (func $svc (param i32 i32 i32 i32) (result i32)))
  (func $copy (param $src i32) (param $dst i32) (param $len i32)
    (block $done (loop $l (br_if $done (i32.eqz (local.get $len)))
      (i32.store8 (local.get $dst) (i32.load8_u (local.get $src)))
      (local.set $src (i32.add (local.get $src) (i32.const 1)))
      (local.set $dst (i32.add (local.get $dst) (i32.const 1)))
      (local.set $len (i32.sub (local.get $len) (i32.const 1)))
      (br $l))))
  (func (export \"lueying_command\") (param i32 i32 i32 i32) (result i32)
    (local $code i32)
    (local.set $code (call $svc (i32.const 8) (i32.const {mlen}) (i32.const {aptr}) (i32.const {alen})))
    (if (i32.lt_s (local.get $code) (i32.const 0))
      (then
        (call $emit (i32.const 2048) (i32.const {elen}) (i32.const 0))
        (return (i32.const 1))))
    (call $copy (i32.const 65536) (i32.const 2048) (local.get $code))
    (call $emit (i32.const 2048) (local.get $code) (i32.const 1))
    (i32.const 0))
  (data (i32.const 8) \"{mtext}\")
  (data (i32.const {aptr}) \"{atext}\")
  (data (i32.const 2048) \"{etext}\"))",
        preamble(),
        mlen = method.len(),
        aptr = args_ptr,
        alen = args.len(),
        elen = "宿主没有回传结果".len(),
        mtext = escape(method),
        atext = escape(args),
        etext = "宿主没有回传结果",
    )
}
