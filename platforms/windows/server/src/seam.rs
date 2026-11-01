//! 重排与联想的提供者缝:内置本地模型是默认实现,外部提供者(旁路进程
//! 里的本地大模型等)是可选实现,配置显式打开才生效(默认关)。
//!
//! 两条缝都不在按键热路径上:
//! - 停顿重排(见 `settle`)本来就异步,缝上的提供者阻塞工作线程无妨,
//!   超时由 [`BypassSettled`] 的调用预算兜住;
//! - 联想缝在 features 的 [`PredictSource`](lueying_features::predict::PredictSource),
//!   契约是亚毫秒级,慢模型自行预取。
//!
//! 提供者连续 3 次失败自动停用回落内置,与脚本/Wasm 层同规。

use std::sync::Arc;

use lueying_bypass::Bypass;

/// 停顿重排的提供者:给左邻上下文与候选文本,返回要置顶的下标。
pub trait SettledRerank: Send + Sync {
    /// 提供者名;日志与停用提示用。
    fn name(&self) -> &str;
    /// `Ok(None)` 表示维持内置顺序;`Err` 计一次连续失败。
    fn promote(&self, context: &str, texts: &[&str]) -> Result<Option<usize>, String>;
}

/// 缝上一次查询的结果;`Builtin` 让工作线程走内置模型。
pub enum SeamOutcome {
    /// 没有提供者、已停用或本次失败:走内置。
    Builtin,
    /// 提供者的结论(含「维持顺序」)。
    Provider(Option<usize>),
}

/// 连续失败停用阈值;与脚本/Wasm 层同一档。
pub const MAX_FAULTS: u32 = 3;

/// 停顿重排的提供者槽位:装配与连续失败计数。
#[derive(Default)]
pub struct SettledSlot {
    provider: Option<Arc<dyn SettledRerank>>,
    faults: u32,
}

impl SettledSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// 换提供者;`None` 回到纯内置,失败计数清零。
    pub fn set(&mut self, provider: Option<Arc<dyn SettledRerank>>) {
        self.provider = provider;
        self.faults = 0;
    }

    /// 是否装着提供者(停用后为 false)。
    pub fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    /// 问一次缝;越界下标与 `Err` 同样计失败,达到阈值停用并回落内置。
    pub fn promote(&mut self, context: &str, texts: &[&str]) -> SeamOutcome {
        let Some(provider) = self.provider.as_ref() else {
            return SeamOutcome::Builtin;
        };
        let name = provider.name().to_string();
        match provider.promote(context, texts) {
            Ok(None) => {
                self.faults = 0;
                SeamOutcome::Provider(None)
            }
            Ok(Some(index)) if index < texts.len() => {
                self.faults = 0;
                SeamOutcome::Provider(Some(index))
            }
            Ok(Some(index)) => {
                self.note_fault(
                    &name,
                    &format!("下标 {index} 越界(共 {} 条候选)", texts.len()),
                );
                SeamOutcome::Builtin
            }
            Err(error) => {
                self.note_fault(&name, &error);
                SeamOutcome::Builtin
            }
        }
    }

    fn note_fault(&mut self, name: &str, error: &str) {
        self.faults += 1;
        if self.faults >= MAX_FAULTS {
            println!("停顿重排 {name}: 连续 {MAX_FAULTS} 次失败({error}),停用并回落内置模型");
            self.provider = None;
        } else {
            println!("停顿重排 {name} 失败: {error}");
        }
    }
}

/// 旁路实现:每次查询拉起旁路进程,JSON 行协议要「置顶下标」。
/// 协议:请求 `{"method":"promote","args":{"context":…,"texts":[…]}}`,
/// 应答 `{"result":{"index":0}}`(`null` = 维持顺序)或 `{"error":…}`。
pub struct BypassSettled {
    bypass: Bypass,
}

impl BypassSettled {
    pub fn new(bypass: Bypass) -> Self {
        Self { bypass }
    }
}

impl SettledRerank for BypassSettled {
    fn name(&self) -> &str {
        "旁路进程"
    }

    fn promote(&self, context: &str, texts: &[&str]) -> Result<Option<usize>, String> {
        let args = serde_json::json!({ "context": context, "texts": texts }).to_string();
        let reply = self.bypass.call("promote", &args)?;
        parse_promote(&reply)
    }
}

/// 解析旁路应答;单独成函数供单测,不必拉真进程。
fn parse_promote(reply: &str) -> Result<Option<usize>, String> {
    let value: serde_json::Value =
        serde_json::from_str(reply).map_err(|error| format!("旁路应答不是 JSON: {error}"))?;
    if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
        return Err(error.to_string());
    }
    let index = value
        .get("result")
        .ok_or("旁路应答缺 result")?
        .get("index")
        .ok_or("旁路应答缺 result.index")?;
    if index.is_null() {
        return Ok(None);
    }
    let index = index.as_u64().ok_or("result.index 不是下标")? as usize;
    Ok(Some(index))
}

#[cfg(test)]
#[path = "seam_tests.rs"]
mod tests;
