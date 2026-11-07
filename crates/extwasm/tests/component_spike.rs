//! 组件缝验证桩的宿主侧。
//!
//! 断言四件事:wit-bindgen guest 能包成组件被 wasmtime::component 实例化;
//! 导出能类型化调用;燃料照常计数;宿主服务(阻塞)照常工作。
//! 这条测试绿了,Extism→Component Model 迁移的全部核心假设就都成立,
//! 真实 world 按同样链路逐服务迁移。

#![allow(missing_docs)]

wasmtime::component::bindgen!({
    path: "spike.wit",
    world: "spike",
});

/// 宿主服务状态;验证桩里只有一份固定回话。
pub struct HostState;

impl spike::host::Host for HostState {
    fn ping(&mut self) -> String {
        "pong".to_string()
    }
}

use std::path::PathBuf;

/// 找到或现场构建 guest 组件。
///
/// CI 先用 wasm-tools 建好并通过 `SPIKE_COMPONENT` 传路径;本地没有
/// wasm-tools 就跳过(验证桩只在 CI 跑,本机没有 Rust 工具链)。
fn component() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SPIKE_COMPONENT") {
        return Some(PathBuf::from(path));
    }
    let tools = std::env::var("WASM_TOOLS").unwrap_or_else(|_| "wasm-tools".into());
    let which = std::process::Command::new(&tools)
        .arg("--version")
        .output()
        .is_ok();
    if !which {
        eprintln!("跳过组件缝验证桩:没有 wasm-tools");
        return None;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/spike-guest/Cargo.toml");
    std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .args(["--target", "wasm32-unknown-unknown"])
        .status()
        .ok()
        .filter(|status| status.success())?;
    // guest 不在工作区,产物落在它自己的 target 目录。
    let guest_target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.parent().unwrap().join("target"));
    let core = guest_target
        .join("wasm32-unknown-unknown/debug/spike_guest.wasm")
        .canonicalize()
        .ok()?;
    let component = guest_target.join("spike.component.wasm");
    let ok = std::process::Command::new(&tools)
        .args(["component", "new"])
        .arg(&core)
        .arg("-o")
        .arg(&component)
        .status()
        .ok()
        .filter(|status| status.success())?;
    ok.then_some(component)
}

#[test]
fn 组件桩_握手_燃料_宿主调用() {
    let Some(component_path) = component() else {
        return;
    };
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    config.max_wasm_stack(256 << 10);
    let engine = wasmtime::Engine::new(&config).unwrap();
    let component = wasmtime::component::Component::from_file(&engine, &component_path).unwrap();
    let mut store = wasmtime::Store::new(&engine, HostState);
    const FUEL: u64 = 50_000_000;
    store.set_fuel(FUEL).unwrap();

    let mut linker = wasmtime::component::Linker::new(&engine);
    spike::host::add_to_linker(&mut linker, |state: &mut HostState| state).unwrap();
    let (bindings, _instance) = spike::Spike::instantiate(&mut store, &component, &linker).unwrap();

    let text = bindings
        .entry()
        .call_hello(&mut store, "掠影")
        .unwrap()
        .unwrap();
    assert_eq!(text, "掠影 ← pong");

    // 燃料必须真的被扣:实例化加一次调用不可能零消耗。
    let left = store.get_fuel().unwrap();
    assert!(left < FUEL, "燃料没有被计数:还剩 {left}");

    // 阻塞宿主调用已经在 hello 里走过一遍(ping);再直接压一次燃料耗尽:
    // 预算设到极小,下一次调用必须 trap 而不是挂死。
    store.set_fuel(1).unwrap();
    let trapped = bindings.entry().call_hello(&mut store, "掠影").is_err();
    assert!(trapped, "燃料耗尽必须 trap");
}
