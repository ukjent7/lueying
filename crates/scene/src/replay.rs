//! 事件模拟器:一份回放脚本推完整条时间线,不敲键盘就能调动画。
//!
//! 脚本与场景同语言(TOML):`[[steps]]` 顺序推进,每步给出走多少秒
//! (`dt`)、可选的信号补丁(不写沿用上一步)与要发的事件。几何由模拟器
//! 合成:候选一行一条,面板宽 360,其余锚矩形随之;同输入必同输出,
//! 回放结果可以逐帧比对。`LUEYING_SCENE_RECORD` 录出的就是这份格式。

use crate::model::{LoadError, Scene};
use crate::runtime::{EventName, Frame, Inputs, Rect, Runtime};

/// 单步 dt 上限;再大会一脚跨过整段动画。
const DT_MAX: f64 = 1.0;
/// 步数上限;与 scene.toml 的节点上限同一个量级。
const STEPS_MAX: usize = 4096;
/// 候选数量上限;与候选窗一页的容量一致。
const CANDIDATES_MAX: usize = 16;
/// 没有击键时的空闲秒数;与窗口层的 `rate_and_idle` 同值。
const IDLE_NEVER: f64 = 999.0;

/// 一份回放脚本;`parse` 产出一个推到底的确定时间线。
pub struct Script {
    steps: Vec<Step>,
}

struct Step {
    dt: f64,
    typing_rate: Option<f64>,
    candidates: Option<usize>,
    page: Option<usize>,
    page_count: Option<usize>,
    mode_chinese: Option<bool>,
    selected: Option<usize>,
    idle: Option<f64>,
    events: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    dt: f64,
    typing_rate: Option<f64>,
    candidates: Option<usize>,
    page: Option<usize>,
    page_count: Option<usize>,
    mode_chinese: Option<bool>,
    selected: Option<usize>,
    idle: Option<f64>,
    #[serde(default)]
    events: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScript {
    #[serde(default)]
    steps: Vec<RawStep>,
}

/// 解析回放脚本;dt、候选数与步数超限都报错,消息带步号。
pub fn parse(file: &str, text: &str) -> Result<Script, Vec<LoadError>> {
    let fail = |message: String| {
        vec![LoadError {
            file: file.to_owned(),
            line: None,
            message,
        }]
    };
    let raw: RawScript = match toml::from_str(text) {
        Ok(raw) => raw,
        Err(error) => return Err(fail(format!("TOML 解析失败:{error}"))),
    };
    if raw.steps.len() > STEPS_MAX {
        return Err(fail(format!(
            "步数 {} 超过上限 {STEPS_MAX}",
            raw.steps.len()
        )));
    }
    let mut steps = Vec::with_capacity(raw.steps.len());
    for (index, step) in raw.steps.into_iter().enumerate() {
        let what = format!("第 {} 步", index + 1);
        if !step.dt.is_finite() {
            return Err(fail(format!("{what}:dt 不是有限数")));
        }
        if !(0.0..=DT_MAX).contains(&step.dt) {
            return Err(fail(format!("{what}:dt = {} 超出 0..={DT_MAX}", step.dt)));
        }
        if let Some(rate) = step.typing_rate
            && !(0.0..=100.0).contains(&rate)
        {
            return Err(fail(format!(
                "{what}:typing_rate = {rate} 超出 0..=100"
            )));
        }
        if let Some(count) = step.candidates
            && count > CANDIDATES_MAX
        {
            return Err(fail(format!(
                "{what}:candidates = {count} 超过 {CANDIDATES_MAX}"
            )));
        }
        if step.page_count == Some(0) {
            return Err(fail(format!("{what}:page_count 不能为 0")));
        }
        if let Some(index) = step.selected
            && index >= CANDIDATES_MAX
        {
            return Err(fail(format!(
                "{what}:selected = {index} 超过 {}",
                CANDIDATES_MAX - 1
            )));
        }
        steps.push(Step {
            dt: step.dt,
            typing_rate: step.typing_rate,
            candidates: step.candidates,
            page: step.page,
            page_count: step.page_count,
            mode_chinese: step.mode_chinese,
            selected: step.selected,
            idle: step.idle,
            events: step.events,
        });
    }
    Ok(Script { steps })
}

/// 事件名与脚本字符串互转的词表;录制与回放共用。
pub fn event_of(name: &str) -> EventName {
    match name {
        "key" => EventName::Key,
        "commit" => EventName::Commit,
        "select" => EventName::Select,
        "page" => EventName::Page,
        "backspace_burst" => EventName::BackspaceBurst,
        "correction" => EventName::Correction,
        "mode_switch" => EventName::ModeSwitch,
        other => EventName::Custom(other.to_owned()),
    }
}

/// [`event_of`] 的反变换;录制时把运行时事件写回脚本字符串。
pub fn name_of(event: &EventName) -> String {
    match event {
        EventName::Key => "key".to_owned(),
        EventName::Commit => "commit".to_owned(),
        EventName::Select => "select".to_owned(),
        EventName::Page => "page".to_owned(),
        EventName::BackspaceBurst => "backspace_burst".to_owned(),
        EventName::Correction => "correction".to_owned(),
        EventName::ModeSwitch => "mode_switch".to_owned(),
        EventName::Custom(name) => name.clone(),
    }
}

/// 合成几何:候选一行一条,面板宽 360,高度随候选数走。
fn geometry(candidates: usize, selected: usize) -> (Rect, Rect, Vec<Rect>, Option<Rect>) {
    const ROW_H: f32 = 40.0;
    let height = 32.0 + 4.0 + candidates.max(1) as f32 * ROW_H + 6.0;
    let panel = [0.0, 0.0, 360.0, height];
    let rects: Vec<Rect> = (0..candidates)
        .map(|index| [4.0, 36.0 + index as f32 * ROW_H, 352.0, 36.0])
        .collect();
    let highlight = rects.get(selected).copied();
    (panel, [0.0, 0.0, 360.0, 32.0], rects, highlight)
}

/// 把脚本从头推到尾;每步出一帧,帧序与步序一致。
pub fn replay(script: &Script, scene: Scene) -> Vec<Frame> {
    let mut runtime = Runtime::new(scene);
    let mut frames = Vec::with_capacity(script.steps.len());
    let mut time = 0.0f64;
    let mut typing_rate = 0.0f64;
    let mut candidates = 0usize;
    let mut page = 0usize;
    let mut page_count = 1usize;
    let mut mode_chinese = true;
    let mut selected = 0usize;
    let mut since_key = IDLE_NEVER;
    for step in &script.steps {
        time += step.dt;
        if let Some(value) = step.typing_rate {
            typing_rate = value;
        }
        if let Some(value) = step.candidates {
            candidates = value;
        }
        if let Some(value) = step.page {
            page = value;
        }
        if let Some(value) = step.page_count {
            page_count = value;
        }
        if let Some(value) = step.mode_chinese {
            mode_chinese = value;
        }
        if let Some(value) = step.selected {
            selected = value;
        }
        since_key = if step.events.iter().any(|name| name == "key") {
            0.0
        } else {
            (since_key + step.dt).min(IDLE_NEVER)
        };
        let idle = step.idle.unwrap_or(since_key);
        let (panel, preedit, rects, highlight) = geometry(candidates, selected);
        let inputs = Inputs {
            typing_rate,
            candidates: rects.len(),
            page,
            page_count,
            mode_chinese,
            selected: selected.min(rects.len().saturating_sub(1)),
            idle,
            panel,
            preedit,
            highlight,
            candidates: &rects,
            caret: [12.0, 36.0, 2.0, 28.0],
            screen: panel,
        };
        let events: Vec<EventName> = step.events.iter().map(|name| event_of(name)).collect();
        frames.push(runtime.tick(time, step.dt, &inputs, &events));
    }
    frames
}
