//! 组件宿主的语义矩阵:往返、燃料耗尽停用、内存上限与编译失败路径。

use std::sync::Arc;

use crate::{MAX_FAULTS, PROBE_WAT, WasmHost, WasmModule};

fn host() -> Arc<WasmHost> {
    Arc::new(WasmHost::new().expect("引擎应能创建"))
}

/// 预烧燃料的死循环组件:`spin(a, b)` 永不返回。
const SPIN_WAT: &str = r#"
(component
  (core module $m
    (func (export "spin") (param i32 i32) (result i32)
      (loop (br 0))
      (unreachable)))
  (core instance $i (instantiate $m))
  (type (func (param "a" u32) (param "b" u32) (result u32)))
  (func $spin (type 0) (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)"#;

/// 按入参分流陷阱的组件:`maybe(a, b) = b / a`,a = 0 时除零陷阱。
/// 连续故障停用与成功清零都要一条实例才能测出「连续」。
const MAYBE_WAT: &str = r#"
(component
  (core module $m
    (func (export "maybe") (param i32 i32) (result i32)
      local.get 1
      local.get 0
      i32.div_u))
  (core instance $i (instantiate $m))
  (type (func (param "a" u32) (param "b" u32) (result u32)))
  (func $maybe (type 0) (canon lift (core func $i "maybe")))
  (export "maybe" (func $maybe))
)"#;

/// 超限增长内存的组件:`grow(pages, 0)` 返回实际增长页数,拒绝时 -1。
const GROW_WAT: &str = r#"
(component
  (core module $m
    (memory (export "mem") 1)
    (func (export "grow") (param i32 i32) (result i32)
      local.get 0
      memory.grow))
  (core instance $i (instantiate $m))
  (type (func (param "a" u32) (param "b" u32) (result u32)))
  (func $grow (type 0) (canon lift (core func $i "grow")))
  (export "grow" (func $grow))
)"#;

#[test]
fn probe_round_trip() {
    let module = WasmModule::compile(&host(), PROBE_WAT.as_bytes()).expect("探针应编译");
    let mut probe = module.instantiate("add").expect("探针应实例化");
    assert_eq!(probe.call(2, 3).expect("调用应成功"), 5);
    assert_eq!(probe.call(0, 0).expect("调用应成功"), 0);
    assert_eq!(probe.call(u32::MAX, 1).expect("调用应成功"), 0);
    let stat = probe.stat();
    assert_eq!(stat.faults, 0);
    assert!(!stat.disabled);
    assert!(stat.fuel_used > 0, "正常调用也应有燃料消耗");
}

#[test]
fn epoch_tick_between_calls_is_harmless() {
    let module = WasmModule::compile(&host(), PROBE_WAT.as_bytes()).expect("探针应编译");
    let mut probe = module.instantiate("add").expect("探针应实例化");
    module.host().tick();
    assert_eq!(probe.call(1, 1).expect("tick 后调用应成功"), 2);
}

#[test]
fn bad_source_is_rejected() {
    let error = WasmModule::compile(&host(), b"(component (broken)").expect_err("坏源码应被拒");
    assert!(error.contains("组件编译失败"));
}

#[test]
fn missing_export_is_rejected() {
    let module = WasmModule::compile(&host(), PROBE_WAT.as_bytes()).expect("探针应编译");
    let error = module.instantiate("nope").expect_err("不存在的导出应被拒");
    assert!(error.contains("没有导出"));
}

#[test]
fn fuel_exhaustion_counts_faults_then_disables() {
    let module = WasmModule::compile(&host(), SPIN_WAT.as_bytes()).expect("死循环应编译");
    let mut probe = module.instantiate("spin").expect("死循环应实例化");
    for attempt in 1..=MAX_FAULTS {
        let error = probe.call(0, 0).expect_err("烧燃料应失败");
        assert!(error.contains("调用失败"), "第 {attempt} 次: {error}");
        assert_eq!(probe.stat().faults, attempt);
        assert_eq!(
            probe.stat().disabled,
            attempt == MAX_FAULTS,
            "停用状态应在第 {MAX_FAULTS} 次失败时翻转"
        );
    }
    // 停用后立即拒绝,不再进入模块。
    let error = probe.call(0, 0).expect_err("停用后应拒绝");
    assert!(error.contains("已停用"), "实际错误 {error}");
}

#[test]
fn success_resets_fault_streak() {
    let module = WasmModule::compile(&host(), MAYBE_WAT.as_bytes()).expect("分流组件应编译");
    let mut probe = module.instantiate("maybe").expect("分流组件应实例化");
    // 故障一次、成功一次:计数清零,两次故障不再停用。
    assert!(probe.call(0, 5).is_err(), "除零应陷阱");
    assert_eq!(probe.call(2, 6).expect("成功应照常"), 3);
    assert_eq!(probe.stat().faults, 0);
    assert!(probe.call(0, 0).is_err());
    assert!(probe.call(0, 0).is_err());
    assert!(!probe.stat().disabled, "中间的成功打断了连续故障");
    assert_eq!(probe.call(3, 9).expect("成功应照常"), 3);
    // 三连故障停用。
    assert!(probe.call(0, 1).is_err());
    assert!(probe.call(0, 1).is_err());
    assert!(probe.call(0, 1).is_err());
    assert!(probe.stat().disabled);
    assert!(probe.call(2, 6).is_err(), "停用后成功路径也要拒绝");
}

#[test]
fn memory_cap_rejects_oversized_growth() {
    let module = WasmModule::compile(&host(), GROW_WAT.as_bytes()).expect("增长组件应编译");
    let mut probe = module.instantiate("grow").expect("增长组件应实例化");
    // 1 页起步 + 200 页 ≈ 12.8 MiB,超过 8 MiB 上限,规范回 -1。
    assert_eq!(probe.call(200, 0).expect("调用应成功"), u32::MAX);
    // 上限内的增长照常:1 页 + 1 页 = 增长 1 页。
    assert_eq!(probe.call(1, 0).expect("调用应成功"), 1);
    assert!(!probe.stat().disabled, "拒绝增长是正常返回,不算故障");
}
