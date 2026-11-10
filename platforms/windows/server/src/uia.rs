//! UI Automation 左邻文本回退。TSF 在 Office/自绘宿主上常读不到文本,
//! 由独立 MTA 线程用 TextPattern 取光标前 128 字;查询不占管道与界面线程,
//! UIA 自身带连接/事务超时,失败只丢本次上下文。

use std::ffi::c_void;
use std::io::Write as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use lueying_core::CONTEXT_KEEP;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    UIA_TextPatternId,
};
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
    if std::env::var_os("LUEYING_DEBUG_UIA").is_some() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::temp_dir().join("lueying-uia.log"))
        {
            let _ = writeln!(file, "{message}");
        }
    }
}

pub(super) fn spawn(state: Arc<State>, rx: mpsc::Receiver<Job>) {
    let spawned = std::thread::Builder::new()
        .name("lueying-uia".into())
        .spawn(move || worker(&state, &rx));
    if spawned.is_err() {
        say("UIA 线程启动失败,左邻文本回退停用");
    }
}

fn worker(state: &Arc<State>, rx: &mpsc::Receiver<Job>) {
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
    while let Ok(job) = rx.recv() {
        say(&format!("UIA: 收到任务 hwnd={:x}", job.hwnd));
        let text =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&automation, &job)))
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

/// 选中文字读取器:独立 MTA 线程常驻,COM 与 UIA 实例只初始化一次。
/// 扩展的 `context` 服务在这里取用户当前选中的文字。
pub(crate) struct SelectionReader {
    tx: Mutex<Option<mpsc::Sender<mpsc::Sender<Option<String>>>>>,
}

impl SelectionReader {
    /// 读一次选中文字。读不到(没有焦点元素、密码框、自家进程、UIA
    /// 报错)返回 `None`;没有选中文字时返回空串。
    pub(crate) fn read(&self) -> Option<String> {
        let tx = self.tx.lock().ok()?.clone()?;
        let (reply, wait) = mpsc::channel();
        tx.send(reply).ok()?;
        wait.recv_timeout(Duration::from_secs(2)).ok().flatten()
    }
}

/// 拉起选中文字读取线程;线程起不来就返回 `None`(调用方按不可用处理)。
pub(crate) fn selection_reader() -> Option<SelectionReader> {
    let (tx, rx) = mpsc::channel::<mpsc::Sender<Option<String>>>();
    let spawned = std::thread::Builder::new()
        .name("lueying-uia-selection".into())
        .spawn(move || {
            if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
                say("选中文字读取线程 COM 初始化失败");
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
                    say(&format!("选中文字 UIA 创建失败: {error}"));
                    unsafe { CoUninitialize() };
                    return;
                }
            };
            if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
                let _ = unsafe { automation2.SetConnectionTimeout(1000) };
                let _ = unsafe { automation2.SetTransactionTimeout(1000) };
            }
            while let Ok(reply) = rx.recv() {
                let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    selection(&automation)
                }))
                .unwrap_or(None);
                let _ = reply.send(text);
            }
            unsafe { CoUninitialize() };
        });
    if spawned.is_err() {
        say("选中文字读取线程启动失败");
        return None;
    }
    Some(SelectionReader {
        tx: Mutex::new(Some(tx)),
    })
}

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
