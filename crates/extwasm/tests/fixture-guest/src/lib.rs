//! 扩展宿主行为测试客人。
//!
//! CI 用 wasm32 目标编出来再 `wasm-tools component new` 打包,扩展宿主的
//! 单元测试与 E2E 通过环境变量加载它,验证命令、报错、trap、燃料、内存
//! 限额、宿主服务与外观的真实组件路径(见 `crates/extwasm/src/test_guest.rs`)。
//!
//! 服务面只用 `notify`:宿主按清单 `needs` 链接,没声明的服务在实例化期
//! 被拒,正好覆盖 needs 门控;组件里其余服务的导入会被裁掉。

use lueying_extguest::{Extension, SceneIn, ScenePatch};
use serde_json::json;

/// 回显前缀;变体 B 加 `[b]`,与变体 A 区分热重载前后的产物。
const MARK: &str = if cfg!(feature = "variant-b") {
    "[b]"
} else {
    ""
};

/// 外观补丁的固定主题色;变体 B 用另一个值,合并顺序测试据此断言后到者覆盖。
const ACCENT: u32 = if cfg!(feature = "variant-b") {
    0x20_40_60
} else {
    0x10_20_30
};

#[derive(Default)]
struct Fixture;

impl Extension for Fixture {
    fn command(&mut self, id: &str, args: &str) -> Result<String, String> {
        match id {
            // 回显:参数原样返回(带变体前缀),命令与热重载断言用。
            "run" | "echo" => Ok(format!("{MARK}{args}")),
            // 带着参数文本报错,错误回传断言用。
            "fail" => Err(format!("{MARK}{args}")),
            // 直接 trap;连续失败停用与 trap 恢复断言用。
            "trap" => unreachable!("测试用 trap"),
            // 死循环;燃料预算打断断言用。
            "spin" => {
                loop {
                    std::hint::black_box(1u64.wrapping_add(1));
                }
            }
            // 一次申请远超上限的内存;内存限额断言用。
            "hog" => {
                let huge = Vec::<u8>::with_capacity(64 << 20);
                std::hint::black_box(huge.len());
                Ok("{}".into())
            }
            // 调一次宿主通知服务;needs 门控与缺提供者断言用。
            "notify" => {
                notify_show("测试客人", args)?;
                Ok(json!({ "notified": true }).to_string())
            }
            other => Err(format!("没有命令 {other}")),
        }
    }

    fn appearance(&mut self, _scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        Ok(Some(ScenePatch {
            accent: Some(ACCENT),
            ..ScenePatch::default()
        }))
    }
}

lueying_extguest::export_component!(Fixture, commands: [run, echo, fail, trap, spin, hog, notify]);
