//! 运行期扩展自检:装一个临时扩展包,验证加载、派发、隐私与高对比度。
//!
//! `lueying-ext-host --selftest <报告>` 跑一次,结论写进报告文件供 win-e2e 断言。
//! guest 是 CI 打包好的组件测试客人(fixture-guest,固定主题色),路径经
//! `LUEYING_SELFTEST_WASM` 传入。

use std::path::{Path, PathBuf};

use lueying_ext::host::{Pack, SceneIn};

/// 测试客人外观补丁的固定主题色(与 fixture-guest 一致)。
const ACCENT: u32 = 0x10_20_30;

/// 清单;id 用进程号,避免和用户自己的扩展撞名。
fn manifest(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"自检扩展","version":"1.0.0","api": 4,"wasm":"extension.wasm","needs":["notify"],"appearance":true}}"#
    )
}

/// 跑自检并写报告;返回进程退出码。
pub fn selftest(report: &Path) -> i32 {
    let result = check();
    let line = match &result {
        Ok(()) => "扩展自检通过:加载、主题色、隐私与高对比度".to_string(),
        Err(error) => format!("扩展自检失败:{error}"),
    };
    println!("{line}");
    if let Some(dir) = report.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(report, format!("{line}\n"));
    i32::from(result.is_err())
}

fn check() -> Result<(), String> {
    let wasm = std::env::var("LUEYING_SELFTEST_WASM")
        .map_err(|_| "LUEYING_SELFTEST_WASM 缺失:没有组件测试客人产物".to_string())
        .and_then(|path| std::fs::read(path).map_err(|e| format!("读测试客人: {e}")))?;
    let id = format!("selftest-{}", std::process::id());
    let dir: PathBuf = std::env::temp_dir().join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建包目录: {e}"))?;
    let written = std::fs::write(dir.join("manifest.json"), manifest(&id))
        .and_then(|()| std::fs::write(dir.join("extension.wasm"), &wasm));
    let outcome = written
        .map_err(|e| format!("写扩展包: {e}"))
        .and_then(|()| check_pack(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

fn check_pack(dir: &Path) -> Result<(), String> {
    let mut pack = Pack::load(dir).map_err(|e| format!("加载: {e}"))?;
    let first = pack
        .dispatch(&SceneIn::default())
        .ok_or("普通场景没有补丁")?;
    if first.accent != Some(ACCENT) {
        return Err(format!("首次派发主题色 {:?}", first.accent));
    }
    if pack
        .dispatch(&SceneIn {
            private: true,
            ..SceneIn::default()
        })
        .is_some()
    {
        return Err("隐私场景仍然派发".into());
    }
    let patch = pack
        .dispatch(&SceneIn::default())
        .ok_or("隐私场景后没有补丁")?;
    if patch.accent != Some(ACCENT) {
        return Err(format!("隐私场景后主题色 {:?}", patch.accent));
    }
    let patch = pack
        .dispatch(&SceneIn {
            high_contrast: true,
            ..SceneIn::default()
        })
        .ok_or("高对比度场景没有补丁")?;
    if patch.accent != Some(ACCENT) {
        return Err(format!("高对比度补丁 {:?}", patch.accent));
    }
    Ok(())
}
