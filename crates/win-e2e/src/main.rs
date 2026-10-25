//! Windows 全链路 E2E:进程内直调 DllRegisterServer 注册 DLL → 激活本输入法 →
//! 自建 RichEdit 宿主(TSF 文本存区)→ SendInput 注入「nihao+空格」→ 读回「你好」,
//! 再切英文打 ASCII、切回中文验证全局模式。产物 target/win-e2e-report.md。

fn main() {
    #[cfg(windows)]
    {
        imp::run();
    }
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{FreeLibrary, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HKL, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SetFocus,
        VIRTUAL_KEY,
    };
    use windows::Win32::UI::TextServices::{
        CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfCompositionView,
        ITfContextComposition, ITfInputProcessorProfileMgr, ITfInputProcessorProfiles,
        ITfThreadMgr, TF_INPUTPROCESSORPROFILE,
        TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE, TF_IPPMF_ENABLEPROFILE, TF_IPPMF_FORPROCESS,
        TF_PROFILETYPE_INPUTPROCESSOR,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, ES_AUTOVSCROLL, ES_MULTILINE,
        GetForegroundWindow, GetWindowTextW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
        SW_SHOWNORMAL, SetForegroundWindow, SetWindowTextW, ShowWindow, TranslateMessage,
        WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW,
        WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
    };
    use windows::core::{GUID, HSTRING, Interface, PCSTR, PCWSTR};

    const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
    const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
    const LANGID_ZH_CN: u16 = 0x0804;

    pub(super) fn run() {
        let report = match real_run() {
            Ok((text, extra)) => {
                format!("# Windows E2E\n\nPASS\n\nEDIT 文本:`{text}`\n\n{extra}\n")
            }
            Err(e) => {
                let tsf = tail_of(std::env::var_os("PINYIN_TSF_LOG").map(std::path::PathBuf::from));
                let server = tail_of(Some(server_log()));
                format!(
                    "# Windows E2E\n\nFAIL\n\n{e}\n\n## TSF 日志(末尾)\n\n```\n{tsf}\n```\n\n## Server 日志(末尾)\n\n```\n{server}\n```\n"
                )
            }
        };
        println!("{report}");
        let _ = std::fs::create_dir_all("target");
        let _ = std::fs::write("target/win-e2e-report.md", &report);
        if !report.contains("PASS") {
            std::process::exit(1);
        }
    }

    fn server_log() -> std::path::PathBuf {
        std::env::current_dir()
            .unwrap_or_default()
            .join("target")
            .join("win-e2e-server.log")
    }

    fn tail_of(path: Option<std::path::PathBuf>) -> String {
        let Some(path) = path else {
            return String::new();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return String::new();
        };
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(160)..].join("\n")
    }

    fn real_run() -> Result<(String, String), String> {
        // 与本机真实安装的 Server 隔离:管道名加测试后缀,DLL 与子进程都继承。
        let suffix = format!("e2e{}", std::process::id());
        let log = std::env::current_dir()
            .unwrap_or_default()
            .join("target")
            .join("win-e2e-tsf.log");
        let _ = std::fs::create_dir_all("target");
        let _ = std::fs::remove_file(&log);
        unsafe {
            // 尚未启动任何线程,设置环境变量不会与并发读取竞争。
            std::env::set_var("PINYIN_PIPE_SUFFIX", &suffix);
            std::env::set_var("PINYIN_TSF_LOG", &log);
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or("无父目录")?.to_path_buf();
        let dll = dir.join("pinyin_tsf.dll");
        if !dll.exists() {
            return Err(format!("找不到 {}", dll.display()));
        }

        // 1. 进程内注册:直调 DllRegisterServer,拿精确 HRESULT(regsvr32 /s 吞掉细节)
        let dll_h = unsafe { LoadLibraryW(&HSTRING::from(dll.as_os_str())) }
            .map_err(|e| format!("LoadLibraryW: {e}"))?;
        type DllServer = unsafe extern "system" fn() -> windows::core::HRESULT;
        let reg_fn = unsafe {
            GetProcAddress(
                dll_h,
                PCSTR::from_raw(c"DllRegisterServer".as_ptr().cast::<u8>()),
            )
        }
        .map(|f| unsafe { std::mem::transmute::<_, DllServer>(f) })
        .ok_or("DLL 未导出 DllRegisterServer")?;
        let hr = unsafe { reg_fn() };
        println!("DllRegisterServer: 0x{:08X}", hr.0 as u32);
        if hr.is_err() {
            return Err(format!("DllRegisterServer: 0x{:08X}", hr.0 as u32));
        }

        // 拉起 server(主词典缺失会退回内置迷你词库,E2E 断言用它)。
        // 用户词库与配置指向本目录,重复运行不受上一轮学习结果影响。
        let data = dir.join("e2e-data");
        std::fs::create_dir_all(&data).map_err(|e| format!("创建 E2E 数据目录: {e}"))?;
        // TSF 在断线时可能懒拉起 Server;测试进程本身也指向隔离目录,
        // 避免清理阶段复活的 Server 读到 runner 的真实配置。
        unsafe {
            std::env::set_var("LOCALAPPDATA", &data);
            std::env::set_var("APPDATA", &data);
        }
        let server_out = server_log();
        let _ = std::fs::remove_file(&server_out);
        let out =
            std::fs::File::create(&server_out).map_err(|e| format!("创建 Server 日志: {e}"))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("复制 Server 日志句柄: {e}"))?;
        let mut server = std::process::Command::new(dir.join("pinyin-server.exe"))
            .env("LOCALAPPDATA", &data)
            .env("APPDATA", &data)
            // CI 没有交互桌面,跳过 GPUI 候选窗,只保留管道服务。
            .env("PINYIN_SERVER_NO_UI", "1")
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .spawn()
            .ok();
        // 给管道建立一点时间,TSF 侧自身还有懒重试兜底。
        std::thread::sleep(Duration::from_millis(600));

        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;

        // 应用必须激活 TSF 线程管理器,按键才会送到文本服务;只选 profile 不够。
        let thread_mgr: ITfThreadMgr =
            unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| format!("CoCreateInstance(ThreadMgr): {e}"))?;
        match unsafe { thread_mgr.Activate() } {
            Ok(_) => println!("ITfThreadMgr::Activate: ok"),
            Err(e) => println!("ITfThreadMgr::Activate: {e}"),
        }

        let result = (|| -> Result<(String, String), String> {
            // 2. 先建宿主窗口并聚焦。TSF 在激活 profile 时需要有一个文档管理器,
            //    否则服务实例刚上线就会被 Deactivate,后续按键全部直通。
            let (hwnd, edit) = create_host_window()?;
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(edit));
            }
            pump_for(Duration::from_millis(150));
            report_tsf_focus();
            // 有窗口焦点后 TSF 才会为这个线程建立文档管理器;等它真正出现,
            // 否则激活 profile 仍可能在上下文就绪前被停用。
            if !wait_for_tsf_focus(Duration::from_secs(3)) {
                return Err("TSF 文档管理器在 3 秒内没有就绪".into());
            }

            // 3. 激活本输入法(本进程)
            let profiles = unsafe {
                CoCreateInstance::<_, windows::core::IUnknown>(
                    &CLSID_TF_InputProcessorProfiles,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
            }
            .map_err(|e| format!("CoCreateInstance: {e}"))?;
            let mgr: ITfInputProcessorProfileMgr = profiles
                .cast()
                .map_err(|e| format!("cast ITfInputProcessorProfileMgr: {e}"))?;

            // 剖面可见性与全貌(诊断 ActivateProfile 0x80070057 用)
            let mut prof = TF_INPUTPROCESSORPROFILE::default();
            match unsafe {
                mgr.GetProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    LANGID_ZH_CN,
                    &CLSID_IME,
                    &GUID_PROFILE,
                    HKL::default(),
                    &mut prof,
                )
            } {
                Ok(()) => println!(
                    "GetProfile: 可见 dwProfileType={} dwFlags=0x{:x}",
                    prof.dwProfileType, prof.dwFlags
                ),
                Err(e) => println!("GetProfile: {e}"),
            }
            if let Ok(enum_) = unsafe { mgr.EnumProfiles(LANGID_ZH_CN) } {
                let mut items = [TF_INPUTPROCESSORPROFILE::default()];
                let mut got = 0u32;
                let mut n = 0usize;
                while unsafe { enum_.Next(&mut items, &mut got) }.is_ok() && got > 0 && n < 64 {
                    n += 1;
                    got = 0;
                }
                println!("EnumProfiles(0x{LANGID_ZH_CN:04x}): {n} 个");
            }

            // 2. 激活本输入法。CI runner 的当前语言通常是 en-US,
            //    ActivateLanguageProfile 会因此返回 E_INVALIDARG;新版接口带
            //    DONTCARECURRENTINPUTLANGUAGE,不依赖宿主当前语言。
            let profiles_api: ITfInputProcessorProfiles = profiles
                .cast()
                .map_err(|e| format!("cast ITfInputProcessorProfiles: {e}"))?;
            match unsafe {
                profiles_api.EnableLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE, true)
            } {
                Ok(()) => println!("EnableLanguageProfile: ok"),
                Err(e) => println!("EnableLanguageProfile: {e}"),
            }
            let mut activated = false;
            match unsafe {
                mgr.ActivateProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    LANGID_ZH_CN,
                    &CLSID_IME,
                    &GUID_PROFILE,
                    HKL::default(),
                    TF_IPPMF_FORPROCESS
                        | TF_IPPMF_ENABLEPROFILE
                        | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
                )
            } {
                Ok(()) => {
                    println!("ActivateProfile(FORPROCESS|ENABLE|DONTLANG): ok");
                    activated = true;
                }
                Err(e) => println!("ActivateProfile(FORPROCESS|ENABLE|DONTLANG): {e}"),
            }
            // 只有新版接口失败时才走老接口。两个都调会再切一次 profile,
            // 已上线的 TSF 实例收到 Deactivate 后不会自动复活,后续按键全部直通。
            if !activated {
                match unsafe {
                    profiles_api.ActivateLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE)
                } {
                    Ok(()) => {
                        println!("ActivateLanguageProfile: ok");
                        activated = true;
                    }
                    Err(e) => println!("ActivateLanguageProfile(当前语言非中文时忽略): {e}"),
                }
            }
            if !activated {
                return Err("无法激活输入法配置".into());
            }
            let mut active_lang = 0u16;
            let mut active_guid = GUID::default();
            match unsafe {
                profiles_api.GetActiveLanguageProfile(
                    &CLSID_IME,
                    &mut active_lang,
                    &mut active_guid,
                )
            } {
                Ok(()) => println!(
                    "GetActiveLanguageProfile: lang=0x{active_lang:04x} guid={active_guid:?}"
                ),
                Err(e) => println!("GetActiveLanguageProfile: {e}"),
            }
            // 激活 profile 到服务实例挂上焦点是异步的;给 TSF 一点消息泵时间。
            pump_for(Duration::from_millis(400));
            report_tsf_focus();
            // SendInput 发给前台窗口;CI 里窗口即使已 Show,也可能没抢到前台。
            let mut foreground = unsafe { GetForegroundWindow() };
            if foreground != hwnd {
                println!("前台窗口不是宿主,重试 SetForegroundWindow");
                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(Some(edit));
                }
                pump_for(Duration::from_millis(300));
                foreground = unsafe { GetForegroundWindow() };
            }
            println!(
                "前台窗口 {}",
                if foreground == hwnd {
                    "是宿主".to_string()
                } else {
                    format!("不是宿主(0x{:X})", foreground.0 as usize)
                }
            );

            // 4. 中文:注入「nihao+空格」
            type_text("nihao")?;
            tap(0x20)?; // 空格
            pump_for(Duration::from_millis(600));

            // 4b. 保留键 Ctrl+.:切换中文标点并持久化到 Server 的 config.ini。
            check_preserved_punct(&data)?;

            // 5. 单击 Shift 切英文,字母应原样进入 RichEdit;再切回中文验证全局模式。
            tap(0x10)?; // VK_SHIFT
            pump_for(Duration::from_millis(200));
            type_text("abc")?;
            pump_for(Duration::from_millis(200));
            tap(0x10)?;
            pump_for(Duration::from_millis(200));
            type_text("nihao")?;
            tap(0x20)?;
            pump_for(Duration::from_millis(600));

            // 6. 读回
            let mut buf = [0u16; 512];
            let n = unsafe { GetWindowTextW(edit, &mut buf) };
            let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if !text.contains("你好abc你好") {
                return Err(format!("RichEdit 文本 `{text}` 不含 你好abc你好"));
            }
            // 全链路提交延迟:空格按下到 RichEdit 出现文本。引擎门限在
            // pinyin-e2e 里卡;这里只抓 TSF/管道/编辑会话导致的卡死或秒级回归。
            let latency = commit_latency(edit, &thread_mgr)?;
            Ok((text, latency))
        })();

        // 6. 清理(尽力)。先停 TSF 再杀 Server:TIP 的 Deactivate 还会发
        //    Focus/Goodbye,顺序反了会让 DLL 懒拉起一个新 Server 留在后台。
        let _ = unsafe { thread_mgr.Deactivate() };
        pump_for(Duration::from_millis(200));
        if let Some(mut child) = server.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::process::Command::new("regsvr32")
            .arg("/s")
            .arg("/u")
            .arg(&dll)
            .status();
        drop(thread_mgr);
        let _ = unsafe { FreeLibrary(dll_h) };
        unsafe { CoUninitialize() };
        result
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

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

    fn report_tsf_focus() {
        match tsf_focus_state() {
            Ok(()) => println!("TSF focus: 文档管理器有顶层上下文"),
            Err(e) => println!("TSF focus: {e}"),
        }
    }

    /// 等 TSF 文档管理器就绪;期间持续泵消息,让 RichEdit 完成焦点/文档绑定。
    fn wait_for_tsf_focus(timeout: Duration) -> bool {
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

    fn edit_text(edit: HWND) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(edit, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// 全链路提交延迟:清空编辑框,打「nihao」,量空格到组合结束、文本落地。
    /// 只设上限抓卡死/秒级回归,精确引擎耗时由 pinyin-e2e 的分位数门禁负责。
    fn commit_latency(edit: HWND, thread_mgr: &ITfThreadMgr) -> Result<String, String> {
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
            "## 全链路提交延迟\n\n| 指标 | 实测 | 门限 |\n|---|---|---|\n| 中位 | {p50:.1} ms | 仅诊断 |\n| p95 | {p95:.1} ms | ≤ 250 |\n| 最大 | {slowest:.1} ms | 仅诊断 |"
        );
        if p95 > 250.0 {
            return Err(format!("{summary}\n\np95 超过 250ms"));
        }
        Ok(summary)
    }

    /// 当前焦点上下文里是否有 TSF 组合;组合结束即提交落地。
    fn composing(thread_mgr: &ITfThreadMgr) -> Result<bool, String> {
        let doc = unsafe { thread_mgr.GetFocus() }.map_err(|e| format!("GetFocus: {e}"))?;
        let context = unsafe { doc.GetTop() }.map_err(|e| format!("GetTop: {e}"))?;
        let composition: ITfContextComposition = context
            .cast()
            .map_err(|e| format!("ITfContextComposition: {e}"))?;
        let list =
            unsafe { composition.EnumCompositions() }.map_err(|e| format!("枚举组合: {e}"))?;
        let mut views: [Option<ITfCompositionView>; 1] = [None];
        let mut fetched = 0u32;
        unsafe { list.Next(&mut views, &mut fetched) }.map_err(|e| format!("读取组合: {e}"))?;
        Ok(fetched > 0)
    }

    fn create_host_window() -> Result<(HWND, HWND), String> {
        // RichEdit 自己实现 ITextStoreACP 并创建 TSF 文档管理器,普通 EDIT 只走 IMM32。
        let msftedit = unsafe { LoadLibraryW(&HSTRING::from("Msftedit.dll")) }
            .map_err(|e| format!("加载 Msftedit.dll: {e}"))?;
        if msftedit.is_invalid() {
            return Err("Msftedit.dll 句柄无效".into());
        }
        let class_name: Vec<u16> = "pinyin-e2e-host\0".encode_utf16().collect();
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

    fn type_text(text: &str) -> Result<(), String> {
        for c in text.chars() {
            tap(vk_of(c))?;
            pump_for(Duration::from_millis(60));
        }
        Ok(())
    }

    fn key_input(vk: u16, up: bool) -> INPUT {
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

    fn tap(vk: u16) -> Result<(), String> {
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
    fn chord(modifier: u16, vk: u16) -> Result<(), String> {
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

    /// Ctrl+. 是 TSF 保留键;切换后 Server 应把 chinese_punct 写进 config.ini。
    fn check_preserved_punct(data: &std::path::Path) -> Result<(), String> {
        const VK_CONTROL: u16 = 0x11;
        const VK_OEM_PERIOD: u16 = 0xBE;
        let config = data.join("pinyin-ime").join("config.ini");
        let expect = |want: &str| -> Result<(), String> {
            // 配置在 Server 线程写完才改名,给 CI 磁盘留足时间。
            let deadline = Instant::now() + Duration::from_millis(1500);
            loop {
                let text = std::fs::read_to_string(&config).unwrap_or_default();
                if text.contains(&format!("chinese_punct={want}")) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "{} 不含 chinese_punct={want}: `{text}`",
                        config.display()
                    ));
                }
                pump_for(Duration::from_millis(25));
            }
        };
        chord(VK_CONTROL, VK_OEM_PERIOD)?;
        expect("false")?;
        chord(VK_CONTROL, VK_OEM_PERIOD)?;
        expect("true")?;
        Ok(())
    }

    /// 泵消息一段时间(异步编辑会话的回调要靠消息循环驱动)。
    fn pump_for(dur: Duration) {
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
}
