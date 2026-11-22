//! UI Automation 读取。TSF 在 Office/自绘宿主上常读不到文本,由一条 MTA 线程
//! 用 TextPattern 取光标前 128 字;扩展的「选中文字」也在它上面读。查询不占管道与
//! 界面线程,UIA 自身带连接/事务超时,失败只丢本次结果。

use std::ffi::c_void;
use std::io::Write as _;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use lueying_core::CONTEXT_KEEP;
use lueying_ext::host::Done;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    UIA_TextPatternId,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::core::Interface;

use crate::imp::State;

pub(super) struct Job {
    pub(super) session: u64,
    pub(super) hwnd: u64,
    pub(super) epoch: u64,
    /// 发起查询时的 Context 序号;同一焦点代次里的新 Context 也会作废旧查询。
    pub(super) seq: u64,
    /// 发起查询时的引擎上下文;回来时已被上屏文本改过就丢弃。
    pub(super) context: String,
}

/// 进程是 GUI 子系统,生产环境的 stdout 没人收;调试与 E2E 统一写临时
/// 目录里的追加日志——runner 与 TSF 懒拉起的实例共享同一个 TEMP,stdout
/// 重定向只覆盖 runner 亲手拉起的那一份。
pub(crate) fn say(message: &str) {
    println!("{message}");
    if std::env::var_os("LUEYING_DEBUG_UIA").is_some()
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::temp_dir().join("lueying-uia.log"))
    {
        let _ = writeln!(file, "{message}");
    }
}

/// 交给 UIA 线程的工作:TSF 读不到左邻文本时的回退、扩展要的选中文字。
pub(super) enum Work {
    Caret(Job),
    Selection(Done),
}

/// 回落宿主的光标查询:工作项很小,走独立线程,不排在 Chrome 的
/// UIA 上下文读取后面。
pub(super) struct AnchorJob {
    pub(super) pid: u32,
    pub(super) hwnd: u64,
    pub(super) delay_ms: u64,
    pub(super) reply: mpsc::Sender<Option<(i32, i32, i32, i32)>>,
}

pub(super) fn spawn_anchor(rx: mpsc::Receiver<AnchorJob>) {
    let spawned = std::thread::Builder::new()
        .name("lueying-anchor".into())
        .spawn(move || anchor_worker(&rx));
    if spawned.is_err() {
        say("光标锚点线程启动失败,候选窗位置可能滞后");
    }
}

fn anchor_worker(rx: &mpsc::Receiver<AnchorJob>) {
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        say("锚点线程 COM 初始化失败");
        return;
    }
    let automation: IUIAutomation = match unsafe {
        CoCreateInstance(
            &CUIAutomation8,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    } {
        Ok(automation) => automation,
        Err(error) => {
            say(&format!("锚点 UIA 创建失败: {error}"));
            unsafe { CoUninitialize() };
            return;
        }
    };
    if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
        let _ = unsafe { automation2.SetConnectionTimeout(1000) };
        let _ = unsafe { automation2.SetTransactionTimeout(1000) };
    }
    // 预热一次跨进程查询:首次要 60ms 左右,别让起句的第一个键等它。
    let _ = unsafe { automation.GetFocusedElement() };
    while let Ok(job) = rx.recv() {
        if job.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(job.delay_ms));
        }
        let rect = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            anchor(&automation, job.pid, job.hwnd)
        }))
        .unwrap_or(None);
        let _ = job.reply.send(rect);
    }
    unsafe { CoUninitialize() };
}

pub(super) fn spawn(state: Arc<State>, rx: mpsc::Receiver<Work>) {
    let spawned = std::thread::Builder::new()
        .name("lueying-uia".into())
        .spawn(move || worker(&state, &rx));
    if spawned.is_err() {
        say("UIA 线程启动失败,左邻文本回退与选中文字停用");
    }
}

fn worker(state: &Arc<State>, rx: &mpsc::Receiver<Work>) {
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
        say("UIA 线程 COM 初始化失败");
        return;
    }
    let automation: IUIAutomation = match unsafe {
        CoCreateInstance(
            &CUIAutomation8,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    } {
        Ok(automation) => automation,
        Err(error) => {
            say(&format!("UIA 创建失败: {error}"));
            unsafe { CoUninitialize() };
            return;
        }
    };
    if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
        // 提供方卡死时不要无限期占住回退线程。
        let _ = unsafe { automation2.SetConnectionTimeout(1000) };
        let _ = unsafe { automation2.SetTransactionTimeout(1000) };
    }
    // 预热一次跨进程查询:首次要 60ms 左右,别让起句的第一个键等它。
    let _ = unsafe { automation.GetFocusedElement() };
    while let Ok(work) = rx.recv() {
        match work {
            Work::Caret(job) => {
                say(&format!("UIA: 收到任务 hwnd={:x}", job.hwnd));
                let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read(&automation, &job)
                }))
                .unwrap_or_else(|error| {
                    let message = error
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| error.downcast_ref::<&str>().map(|s| (*s).to_string()))
                        .unwrap_or_else(|| "非字符串 panic".into());
                    say(&format!("UIA 读取 panic: {message}"));
                    None
                });
                if let Some(text) = text {
                    crate::imp::apply_uia_context(state, &job, &text);
                }
            }
            Work::Selection(done) => {
                let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    selection(&automation)
                }))
                .unwrap_or(None);
                done.complete(match text {
                    Some(text) => Ok(serde_json::json!({ "text": text }).to_string()),
                    None => Err("读不到选中文字(没有焦点元素或被系统拒绝)".to_string()),
                });
            }
        }
    }
    unsafe { CoUninitialize() };
}

fn read(automation: &IUIAutomation, job: &Job) -> Option<String> {
    let host_pid = (job.session >> 32) as u32;
    let hwnd = HWND(job.hwnd as usize as *mut c_void);
    debug(&format!("读取开始 host_pid={host_pid} hwnd={:x}", job.hwnd));
    // 先看真正的焦点元素,自绘宿主常把 TextPattern 挂在它上面。
    let focused = unsafe { automation.GetFocusedElement() }.ok();
    if let Some(element) = focused.as_ref()
        && pid(element) == Some(host_pid)
        && password(element)
    {
        // 焦点本身就是密码框,不再从窗口层找文本。
        debug("焦点是密码框,跳过");
        return None;
    }
    let mut pattern = focused
        .as_ref()
        .and_then(|element| text_pattern(element, host_pid));
    // 焦点元素没有 TextPattern 时再按窗口句柄找一次。
    if pattern.is_none()
        && !hwnd.is_invalid()
        && let Ok(element) = unsafe { automation.ElementFromHandle(hwnd) }
    {
        pattern = text_pattern(&element, host_pid);
    }
    let Some(pattern) = pattern else {
        debug("焦点元素没有 TextPattern");
        return None;
    };
    let ranges = unsafe { pattern.GetSelection() }.ok()?;
    if unsafe { ranges.Length() }.ok()? <= 0 {
        return None;
    }
    let caret = unsafe { ranges.GetElement(0) }.ok()?;
    let before = unsafe { caret.Clone() }.ok()?;
    unsafe {
        before
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &caret,
                TextPatternRangeEndpoint_Start,
            )
            .ok()?;
    }
    let moved = unsafe {
        before
            .MoveEndpointByUnit(
                TextPatternRangeEndpoint_Start,
                TextUnit_Character,
                -(CONTEXT_KEEP as i32),
            )
            .ok()?
    };
    if moved == 0 {
        return Some(String::new());
    }
    let text = unsafe { before.GetText((CONTEXT_KEEP * 2) as i32) }.ok()?;
    let text = sanitize(&text.to_string());
    debug(&format!("读到 {} 字", text.chars().count()));
    Some(text)
}

/// 回落宿主的候选窗锚点:焦点元素的文本光标矩形。XAML 的 RichEditBox
/// 不建 Win32 光标(`GetGUIThreadInfo` 拿不到),UIA 的 TextPattern 才有。
fn anchor(automation: &IUIAutomation, host_pid: u32, hwnd: u64) -> Option<(i32, i32, i32, i32)> {
    let window = HWND(hwnd as usize as *mut c_void);
    let element = unsafe { automation.GetFocusedElement() }.ok()?;
    if pid(&element) != Some(host_pid) {
        return None;
    }
    let pattern = text_pattern(&element, host_pid)?;
    let rect = caret_rect(&pattern)?;
    // 光标必须落在焦点元素与前台窗口里:动画中的中间位置、别的控件
    // 报上来的矩形都不采纳,候选窗不会跳到奇怪的地方。
    let bounds = unsafe { element.CurrentBoundingRectangle() }.ok()?;
    inside(rect, (bounds.left, bounds.top, bounds.right, bounds.bottom))?;
    let mut window_rect = RECT::default();
    unsafe { GetWindowRect(window, &mut window_rect) }.ok()?;
    inside(
        rect,
        (
            window_rect.left,
            window_rect.top,
            window_rect.right,
            window_rect.bottom,
        ),
    )?;
    Some(rect)
}

/// 矩形是否落在外框内;留 4px 余量给光标下缘的字体下沉。
fn inside(rect: (i32, i32, i32, i32), outer: (i32, i32, i32, i32)) -> Option<()> {
    let (left, top, right, bottom) = rect;
    let (outer_left, outer_top, outer_right, outer_bottom) = outer;
    (left >= outer_left - 4
        && top >= outer_top - 4
        && right <= outer_right + 4
        && bottom <= outer_bottom + 4)
        .then_some(())
}

fn caret_rect(pattern: &IUIAutomationTextPattern) -> Option<(i32, i32, i32, i32)> {
    let ranges = unsafe { pattern.GetSelection() }.ok()?;
    if unsafe { ranges.Length() }.ok()? <= 0 {
        return None;
    }
    let range = unsafe { ranges.GetElement(0) }.ok()?;
    let array = unsafe { range.GetBoundingRectangles() }.ok()?;
    first_rect(array)
}

/// `GetBoundingRectangles` 返回 [left, top, width, height, …] 的 double 数组,
/// 调用方负责销毁;只取第一块,光标是退化区间时它就是插入点。
fn first_rect(array: *mut SAFEARRAY) -> Option<(i32, i32, i32, i32)> {
    let mut rect = None;
    unsafe {
        if let (Ok(lower), Ok(upper)) = (SafeArrayGetLBound(array, 1), SafeArrayGetUBound(array, 1))
            && upper - lower + 1 >= 4
        {
            let mut data = std::ptr::null_mut();
            if SafeArrayAccessData(array, &mut data).is_ok() {
                let values = std::slice::from_raw_parts(data as *const f64, 4);
                rect = normalize((
                    values[0].round() as i32,
                    values[1].round() as i32,
                    (values[0] + values[2]).round() as i32,
                    (values[1] + values[3]).round() as i32,
                ));
                let _ = SafeArrayUnaccessData(array);
            }
        }
        let _ = SafeArrayDestroy(array);
    }
    rect
}

/// 去掉空矩形与明显越界的坐标;零宽/零高补到 1,后续摆位只认正尺寸。
fn normalize(rect: (i32, i32, i32, i32)) -> Option<(i32, i32, i32, i32)> {
    let (left, top, right, bottom) = rect;
    if !(-1_000_000..=1_000_000).contains(&left)
        || !(-1_000_000..=1_000_000).contains(&top)
        || !(-1_000_000..=1_000_000).contains(&right)
        || !(-1_000_000..=1_000_000).contains(&bottom)
        || right < left
        || bottom < top
    {
        return None;
    }
    Some((left, top, right.max(left + 1), bottom.max(top + 1)))
}

fn pid(element: &IUIAutomationElement) -> Option<u32> {
    unsafe { element.CurrentProcessId() }
        .ok()
        .map(|pid| pid as u32)
}

/// IsPassword 读不到时按私密处理;宁可少读一次上下文。
fn password(element: &IUIAutomationElement) -> bool {
    match unsafe { element.CurrentIsPassword() } {
        Ok(value) => value.as_bool(),
        Err(_) => true,
    }
}

/// 只接受本会话宿主进程里的非密码元素,避免读到别的窗口或密码框。
fn text_pattern(element: &IUIAutomationElement, host_pid: u32) -> Option<IUIAutomationTextPattern> {
    if pid(element)? != host_pid || password(element) {
        return None;
    }
    let pattern: IUIAutomationTextPattern =
        unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }.ok()?;
    Some(pattern)
}

fn sanitize(text: &str) -> String {
    let mut text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let count = text.chars().count();
    if count > CONTEXT_KEEP {
        text = text.chars().skip(count - CONTEXT_KEEP).collect();
    }
    text
}

fn debug(message: &str) {
    if std::env::var_os("LUEYING_DEBUG_UIA").is_some() {
        say(&format!("UIA: {message}"));
    }
}

/// 选中文字的上限(字符);够用,又不至于把整篇文档塞进扩展。
const SELECTION_LIMIT: usize = 8192;

/// 取焦点元素里选中的文字;密码框与自家进程一律不读。
fn selection(automation: &IUIAutomation) -> Option<String> {
    let element = unsafe { automation.GetFocusedElement() }.ok()?;
    if password(&element) {
        return None;
    }
    if pid(&element) == Some(std::process::id()) {
        return None;
    }
    let pattern: IUIAutomationTextPattern =
        unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }.ok()?;
    let ranges = unsafe { pattern.GetSelection() }.ok()?;
    let mut text = String::new();
    // 选区一般是一段;少数宿主会报多段,取前几段就够,多了也不给。
    for index in 0..8 {
        let Ok(range) = (unsafe { ranges.GetElement(index) }) else {
            break;
        };
        let Ok(chunk) = (unsafe { range.GetText(-1) }) else {
            break;
        };
        text.push_str(&chunk.to_string());
    }
    Some(trim_selection(&text))
}

/// 选中文字里的控制字符换成空格;换行与制表保留,再截到上限。
fn trim_selection(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let count = out.chars().count();
    if count > SELECTION_LIMIT {
        out = out.chars().skip(count - SELECTION_LIMIT).collect();
    }
    out
}
