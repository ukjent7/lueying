//! 运行期脚本自检:建一个临时扩展包,验证加载、派发、隐私与高对比度。
//!
//! 只在 `LUEYING_SKIA_SELFTEST` 下跑(CI),结论写日志供 win-e2e 断言。

use std::path::Path;

use lueying_extjs::{Pack, SceneIn};
use lueying_render::effect::{Slot, Uniforms};
use lueying_server::config;

/// 扩展包内容:每次派发计数加一,主题色随计数变化,便于断言隐私场景没被调用。
const SCRIPT: &str = r#"let n = 0;
lueying.register(function (state) {
  n += 1;
  return {
    accent: 0x100000 + n,
    effects: {
      background: "half4 main(float2 p) { return half4(0.1, 0.2, 0.3, 1.0); }"
    }
  };
});
"#;

/// 清单;id 用进程号,避免和用户自己的扩展撞名。
fn manifest(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"自检扩展","version":"1.0.0","api":1}}"#)
}

pub(super) fn run() {
    match check() {
        Ok(()) => println!("扩展自检通过:加载、主题色、效果、隐私与高对比度"),
        Err(e) => println!("扩展自检失败:{e}"),
    }
}

fn check() -> Result<(), String> {
    let root = config::extensions_dir().ok_or("APPDATA 缺失")?;
    let id = format!("selftest-{}", std::process::id());
    let dir = root.join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建包目录: {e}"))?;
    let written = std::fs::write(dir.join("manifest.json"), manifest(&id))
        .and_then(|()| std::fs::write(dir.join("index.js"), SCRIPT));
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
    if first.accent != Some(0x10_00_01) {
        return Err(format!("首次派发主题色 {:?}", first.accent));
    }
    if first
        .effects
        .as_ref()
        .and_then(|e| e.background.as_deref())
        .is_none()
    {
        return Err("首次派发没有背景效果".into());
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
    // 隐私场景没有调用脚本,计数只前进一次。
    if patch.accent != Some(0x10_00_02) {
        return Err(format!("隐私场景后主题色 {:?}", patch.accent));
    }
    let patch = pack
        .dispatch(&SceneIn {
            high_contrast: true,
            ..SceneIn::default()
        })
        .ok_or("高对比度场景没有补丁")?;
    if patch.accent != Some(0x10_00_03) || patch.effects.is_some() {
        return Err(format!(
            "高对比度补丁 {:?} / 效果 {:?}",
            patch.accent, patch.effects
        ));
    }
    // 脚本给的效果源码要真的能编译成 Skia shader,与候选窗同一条路径。
    let fx = crate::ui_skia_script::ScriptFx::update(None, first);
    let uniforms = Uniforms {
        size: [32.0, 32.0],
        time: 0.0,
        anim: 1.0,
        accent: [1.0, 0.0, 0.0, 0.0],
    };
    if fx
        .shader(Slot::Background, &uniforms, (0.0, 0.0), 1.0)
        .is_none()
    {
        return Err("脚本效果没有编译成 shader".into());
    }
    Ok(())
}
