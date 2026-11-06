//! TSF/UIA 测量与按键注入:焦点、组句、延迟、内存与宿主窗口。
//!
//! 从 imp.rs 挪出的测量辅助;调度与汇总仍在父模块 real_run。

use super::*;

/// 看测试宿主线程有没有 TSF 文档管理器;没有说明 EDIT 不是 TSF 上下文,组句无法落地。
fn tsf_focus_state() -> Result<(), String> {
    let mgr: ITfThreadMgr =
        unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("线程管理器创建失败: {e}"))?;
    let dm = unsafe { mgr.GetFocus() }.map_err(|e| format!("无文档管理器: {e}"))?;
    unsafe { dm.GetTop() }
        .map(|_| ())
        .map_err(|e| format!("文档管理器无上下文: {e}"))
}

pub(crate) fn report_tsf_focus() {
    match tsf_focus_state() {
        Ok(()) => println!("TSF focus: 文档管理器有顶层上下文"),
        Err(e) => println!("TSF focus: {e}"),
    }
}

/// 等 TSF 文档管理器就绪;期间持续泵消息,让 RichEdit 完成焦点/文档绑定。
pub(crate) fn wait_for_tsf_focus(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if tsf_focus_state().is_ok() {
            println!("TSF focus: 就绪");
            return true;
        }
        if Instant::now() >= deadline {
            report_tsf_focus();
            return false;
        }
        pump_for(Duration::from_millis(100));
    }
}

pub(crate) fn edit_text(edit: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(edit, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 全链路提交延迟:清空编辑框,打「nihao」,量空格到组合结束、文本落地。
/// 只设上限抓卡死/秒级回归,精确引擎耗时由 lueying-e2e 的分位数门禁负责。
/// 全链路提交 p95 门限:2026-10-31 连续四次 Windows E2E 实测 14.7–15.1 ms。
/// 留约 4 倍余量覆盖共享 runner 抖动,同时能抓住 TSF/管道/编辑会话的秒级回归。
const COMMIT_P95_LIMIT_MS: f64 = 60.0;
/// 每键往返 p95 门限:同期实测 4.6–9.5 ms;60 ms 是用户能感知卡顿的量级。
const KEY_P95_LIMIT_MS: f64 = 60.0;
/// Server 峰值工作集门限:产品上限 160 MB,覆盖模型与缓存增长,
/// 又能抓住泄漏或大对象常驻。
const SERVER_MEMORY_LIMIT_MB: u64 = 160;

pub(crate) fn commit_latency(edit: HWND, thread_mgr: &ITfThreadMgr) -> Result<String, String> {
    const ROUNDS: usize = 5;
    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        unsafe {
            let _ = SetWindowTextW(edit, &HSTRING::from(""));
        }
        pump_for(Duration::from_millis(80));
        type_text("nihao")?;
        pump_for(Duration::from_millis(120));
        if !composing(thread_mgr)? {
            return Err(format!("第 {} 轮打字后没有组合", round + 1));
        }
        let start = Instant::now();
        tap(0x20)?;
        let deadline = start + Duration::from_millis(800);
        // 内联预演在空格前就写进了编辑框,不能等文本变化;等组合结束。
        while composing(thread_mgr)? {
            if Instant::now() >= deadline {
                return Err(format!("第 {} 轮提交超过 800ms", round + 1));
            }
            pump_for(Duration::from_millis(5));
        }
        let text = edit_text(edit);
        if !text.contains("你好") {
            return Err(format!("第 {} 轮上屏 `{text}` 不含 你好", round + 1));
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let pick = |p: f64| samples[((p * (ROUNDS - 1) as f64).round() as usize).min(ROUNDS - 1)];
    let (p50, p95) = (pick(0.5), pick(0.95));
    let slowest = samples.last().copied().unwrap_or(0.0);
    let summary = format!(
        "## 全链路提交延迟\n\n| 指标 | 实测 | 门限 |\n|---|---|---|\n| 中位 | {p50:.1} ms | 仅诊断 |\n| p95 | {p95:.1} ms | ≤ {COMMIT_P95_LIMIT_MS:.0} |\n| 最大 | {slowest:.1} ms | 仅诊断 |"
    );
    if p95 > COMMIT_P95_LIMIT_MS {
        return Err(format!("{summary}\n\np95 超过 {COMMIT_P95_LIMIT_MS:.0}ms"));
    }
    Ok(summary)
}

/// 每键延迟:逐键等 TSF 日志里的「应用 vk=」行前进(每键恰好一行,
/// 键事件已被 DLL 交给 Server 并把组句写回)。BeginUIElement 与首次
/// 候选更新不保证触发 UpdateUIElement,不能拿 UI 元素更新当观测点。
/// 引擎门限在 lueying-e2e 卡,这里只抓组合通道整体停摆或秒级回归。
pub(crate) fn key_latency(edit: HWND, tsf_log: &std::path::Path) -> Result<String, String> {
    const ROUNDS: usize = 3;
    const KEYS: [char; 5] = ['w', 'o', 'a', 'i', 'n'];
    let applied = || {
        std::fs::read_to_string(tsf_log)
            .unwrap_or_default()
            .matches("应用 vk=")
            .count()
    };
    let mut samples = Vec::with_capacity(ROUNDS * KEYS.len());
    for round in 0..ROUNDS {
        unsafe {
            let _ = SetWindowTextW(edit, &HSTRING::from(""));
        }
        pump_for(Duration::from_millis(80));
        for (i, ch) in KEYS.iter().enumerate() {
            let before = applied();
            let start = Instant::now();
            tap(vk_of(*ch))?;
            let deadline = start + Duration::from_millis(800);
            while applied() == before {
                if Instant::now() >= deadline {
                    return Err(format!(
                        "第 {} 轮第 {} 键 `{}` 后 800ms 组合通道无响应",
                        round + 1,
                        i + 1,
                        ch
                    ));
                }
                pump_for(Duration::from_millis(2));
            }
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            pump_for(Duration::from_millis(40));
        }
        tap(0x20)?; // 空格上屏,结束组合,下一轮从头开始
        pump_for(Duration::from_millis(300));
    }
    samples.sort_by(f64::total_cmp);
    let pick = |p: f64| {
        samples[((p * (samples.len() - 1) as f64).round() as usize).min(samples.len() - 1)]
    };
    let (p50, p95) = (pick(0.5), pick(0.95));
    let slowest = samples.last().copied().unwrap_or(0.0);
    let summary = format!(
        "## 每键往返延迟\n\n| 指标 | 实测 | 门限 |\n|---|---|---|\n| 中位 | {p50:.1} ms | 仅诊断 |\n| p95 | {p95:.1} ms | ≤ {KEY_P95_LIMIT_MS:.0} |\n| 最大 | {slowest:.1} ms | 仅诊断 |"
    );
    if p95 > KEY_P95_LIMIT_MS {
        return Err(format!("{summary}\n\np95 超过 {KEY_P95_LIMIT_MS:.0}ms"));
    }
    Ok(summary)
}

/// 一个进程的峰值工作集;内存基线与 Slint 验证共用。
pub(crate) fn peak_working_set(pid: u32) -> Result<u64, String> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| format!("OpenProcess({pid}): {e}"))?;
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let result = unsafe { GetProcessMemoryInfo(process, &mut counters, counters.cb) }
        .map(|_| counters.PeakWorkingSetSize as u64)
        .map_err(|e| format!("GetProcessMemoryInfo({pid}): {e}"));
    let _ = unsafe { CloseHandle(process) };
    result
}

/// Server 工作集基线:优先测 runner 自己拉起的进程,不在了再按 exe 名找
/// (TSF 断线后会懒拉起新实例)。只抓失控级增长,细粒度波动仅记录。
pub(crate) fn server_memory(server_pid: u32) -> Result<String, String> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let peak = peak_working_set;
    let pid_by_name = |name: &str| -> Result<u32, String> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| format!("CreateToolhelp32Snapshot: {e}"))?;
        let target: Vec<u16> = format!("{name}\0").encode_utf16().collect();
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = 0u32;
        let mut walking = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        while walking {
            if entry.szExeFile[..target.len()] == target[..] {
                found = entry.th32ProcessID;
                break;
            }
            walking = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }
        let _ = unsafe { CloseHandle(snapshot) };
        if found == 0 {
            return Err(format!("进程表里找不到 {name}"));
        }
        Ok(found)
    };

    let (pid, peak) = if server_pid != 0 {
        match peak(server_pid) {
            Ok(bytes) => (server_pid, bytes),
            Err(e) => {
                println!("按子进程 PID 测内存失败,按 exe 名重找: {e}");
                let pid = pid_by_name("lueying-server.exe")?;
                (pid, peak(pid)?)
            }
        }
    } else {
        let pid = pid_by_name("lueying-server.exe")?;
        (pid, peak(pid)?)
    };
    let mb = peak as f64 / 1024.0 / 1024.0;
    let summary = format!(
        "## Server 内存基线\n\n| 指标 | 实测 | 门限 |\n|---|---|---|\n| 峰值工作集(PID {pid}) | {mb:.1} MB | ≤ {SERVER_MEMORY_LIMIT_MB} |"
    );
    if peak > SERVER_MEMORY_LIMIT_MB * 1024 * 1024 {
        return Err(format!(
            "{summary}\n\n峰值工作集超过 {SERVER_MEMORY_LIMIT_MB}MB"
        ));
    }
    Ok(summary)
}

/// 当前焦点上下文里是否有 TSF 组合;组合结束即提交落地。
pub(crate) fn composing(thread_mgr: &ITfThreadMgr) -> Result<bool, String> {
    let doc = unsafe { thread_mgr.GetFocus() }.map_err(|e| format!("GetFocus: {e}"))?;
    let context = unsafe { doc.GetTop() }.map_err(|e| format!("GetTop: {e}"))?;
    let composition: ITfContextComposition = context
        .cast()
        .map_err(|e| format!("ITfContextComposition: {e}"))?;
    let list = unsafe { composition.EnumCompositions() }.map_err(|e| format!("枚举组合: {e}"))?;
    let mut views: [Option<ITfCompositionView>; 1] = [None];
    let mut fetched = 0u32;
    unsafe { list.Next(&mut views, &mut fetched) }.map_err(|e| format!("读取组合: {e}"))?;
    Ok(fetched > 0)
}

pub(crate) fn create_host_window() -> Result<(HWND, HWND), String> {
    // RichEdit 自己实现 ITextStoreACP 并创建 TSF 文档管理器,普通 EDIT 只走 IMM32。
    let msftedit = unsafe { LoadLibraryW(&HSTRING::from("Msftedit.dll")) }
        .map_err(|e| format!("加载 Msftedit.dll: {e}"))?;
    if msftedit.is_invalid() {
        return Err("Msftedit.dll 句柄无效".into());
    }
    let class_name: Vec<u16> = "lueying-e2e-host\0".encode_utf16().collect();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        hInstance: HINSTANCE::default(),
        ..Default::default()
    };
    let atom = unsafe { RegisterClassW(&wc) };
    if atom == 0 {
        return Err("RegisterClassW 失败".into());
    }
    let title: Vec<u16> = "e2e\0".encode_utf16().collect();
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            100,
            100,
            600,
            300,
            None,
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW: {e}"))?;
    let edit_name: Vec<u16> = "RICHEDIT50W\0".encode_utf16().collect();
    let edit = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            PCWSTR(edit_name.as_ptr()),
            PCWSTR::null(),
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE(ES_MULTILINE as u32)
                | WINDOW_STYLE(ES_AUTOVSCROLL as u32),
            10,
            10,
            550,
            200,
            Some(hwnd),
            None,
            None,
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW RichEdit: {e}"))?;
    Ok((hwnd, edit))
}

fn vk_of(c: char) -> u16 {
    c.to_ascii_uppercase() as u16
}

pub(crate) fn type_text(text: &str) -> Result<(), String> {
    for c in text.chars() {
        tap(vk_of(c))?;
        pump_for(Duration::from_millis(60));
    }
    Ok(())
}

pub(crate) fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub(crate) fn tap(vk: u16) -> Result<(), String> {
    let sent = unsafe {
        SendInput(
            &[key_input(vk, false), key_input(vk, true)],
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != 2 {
        return Err(format!("SendInput 只发出 {sent}"));
    }
    Ok(())
}

/// 修饰键 + 普通键的组合(按下修饰键、按键、抬键、抬修饰键)。
pub(crate) fn chord(modifier: u16, vk: u16) -> Result<(), String> {
    let sent = unsafe {
        SendInput(
            &[
                key_input(modifier, false),
                key_input(vk, false),
                key_input(vk, true),
                key_input(modifier, true),
            ],
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != 4 {
        return Err(format!("SendInput 只发出 {sent}"));
    }
    Ok(())
}

/// 泵消息一段时间(异步编辑会话的回调要靠消息循环驱动)。
pub(crate) fn pump_for(dur: Duration) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        let mut msg = MSG::default();
        unsafe {
            if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
