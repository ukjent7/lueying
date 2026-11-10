#[path = "win_e2e_checks.rs"]
mod checks;
#[path = "pipe_gate.rs"]
mod pipe_gate;

#[path = "win_e2e_measure.rs"]
mod measure;

use checks::*;

pub(crate) use measure::{
    chord, commit_latency, composing, create_host_window, edit_text, key_input, key_latency,
    peak_working_set, pump_for, report_tsf_focus, server_memory, tap, type_text,
    wait_for_tsf_focus,
};

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, FreeLibrary, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM,
};
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
    CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfCompositionView, ITfContextComposition,
    ITfInputProcessorProfileMgr, ITfInputProcessorProfiles, ITfSource, ITfThreadMgr,
    ITfUIElementSink, ITfUIElementSink_Impl, TF_INPUTPROCESSORPROFILE,
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
use windows::core::{GUID, HSTRING, Interface, PCSTR, PCWSTR, implement};

const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
const LANGID_ZH_CN: u16 = 0x0804;

#[derive(Default)]
struct UiEvents {
    began: bool,
    ended: bool,
    updates: u32,
    id: u32,
}

#[implement(ITfUIElementSink)]
struct UiSink {
    events: Rc<RefCell<UiEvents>>,
}

impl ITfUIElementSink_Impl for UiSink_Impl {
    fn BeginUIElement(
        &self,
        dwuielementid: u32,
        pbshow: *mut windows::core::BOOL,
    ) -> windows::core::Result<()> {
        let mut events = self.events.borrow_mut();
        events.began = true;
        events.id = dwuielementid;
        // 宿主保留输入法自己的候选窗;服务端候选窗在 CI 里被 NO_UI 关掉,
        // 这里只验证 UI 元素确实登记并能被读走。
        if !pbshow.is_null() {
            unsafe { *pbshow = TRUE };
        }
        Ok(())
    }

    fn UpdateUIElement(&self, _dwuielementid: u32) -> windows::core::Result<()> {
        self.events.borrow_mut().updates += 1;
        Ok(())
    }

    fn EndUIElement(&self, _dwuielementid: u32) -> windows::core::Result<()> {
        self.events.borrow_mut().ended = true;
        Ok(())
    }
}

pub(super) fn run() {
    let report = match real_run() {
        Ok((text, extra)) => {
            format!("# Windows E2E\n\nPASS\n\nEDIT 文本:`{text}`\n\n{extra}\n")
        }
        Err(e) => {
            let tsf = tail_of(std::env::var_os("LUEYING_TSF_LOG").map(std::path::PathBuf::from));
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

/// 把官方 shot-ocr 扩展装进隔离的 APPDATA,并写一份启用它的配置。
/// 清单编译期嵌入,Wasm guest 是 CI 构建产物,运行期从仓库路径读;
/// 发布包里随安装包分发。安装器会写同意记录;E2E 直接搭目录,这里补上。
fn install_shot_ocr(data: &std::path::Path) -> Result<(), String> {
    const MANIFEST: &str = include_str!("../../../../extensions/shot-ocr/manifest.json");
    let wasm = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../extensions/shot-ocr/extension.wasm"
    ))
    .map_err(|_| {
        "缺少 extensions/shot-ocr/extension.wasm;先 cargo build -p shot-ocr --target wasm32-unknown-unknown --release"
            .to_string()
    })?;
    let extensions = data.join("lueying").join("extensions");
    let dir = extensions.join("shot-ocr");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建扩展目录: {e}"))?;
    std::fs::write(dir.join("manifest.json"), MANIFEST)
        .and_then(|()| std::fs::write(dir.join("extension.wasm"), &wasm))
        .map_err(|e| format!("写扩展文件: {e}"))?;
    let manifest = lueying_extmeta::Manifest::parse(MANIFEST.as_bytes())
        .map_err(|e| format!("解析清单: {e}"))?;
    let mut book = lueying_extmeta::consent::ConsentBook::default();
    book.approve(&dir, &manifest, 0)
        .map_err(|e| format!("写同意记录: {e}"))?;
    book.save(&extensions.join("consent.json"))
        .map_err(|e| format!("保存同意记录: {e}"))?;
    std::fs::write(
        data.join("lueying").join("config.toml"),
        "extensions = [\"shot-ocr\"]\npanel_hotkey = \"off\"\n\n[bindings]\n\"Ctrl+Alt+F9\" = \"shot-ocr.run\"\n",
    )
    .map_err(|e| format!("写 E2E 配置: {e}"))?;
    Ok(())
}

/// 设置窗自检:Slint 组件实例化、页面模型灌入与关键控件在位。
///
/// 设置窗是独立进程,自检铺模型、建组件、回读属性;报告写进文件,
/// 这里原样带进 E2E 报告。
fn settings_selftest(dir: &std::path::Path) -> Result<String, String> {
    let exe = dir.join("lueying-settings.exe");
    if !exe.exists() {
        return Err(format!("没有 {}", exe.display()));
    }
    let report = std::env::current_dir()
        .unwrap_or_default()
        .join("target")
        .join("lueying-settings-selftest.txt");
    let _ = std::fs::create_dir_all("target");
    let _ = std::fs::remove_file(&report);
    let output = std::process::Command::new(&exe)
        .arg("--selftest")
        .arg(&report)
        .output()
        .map_err(|e| format!("启动设置窗自检失败: {e}"))?;
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    if !output.status.success() {
        return Err(format!("设置窗自检失败: {text}"));
    }
    if !text.contains("结果:通过") {
        return Err(format!("设置窗自检没有结论: {text}"));
    }
    Ok(format!("## 设置窗自检\n\n{text}\n"))
}

/// 命令面板自检:独立 Server 进程建可获焦窗口、注入按键、验证焦点归还。
/// 面板会短暂抢前台,放在所有 TSF 文本断言之后跑。
fn panel_selftest(dir: &std::path::Path) -> Result<String, String> {
    let exe = dir.join("lueying-server.exe");
    if !exe.exists() {
        return Err(format!("没有 {}", exe.display()));
    }
    let report = std::env::current_dir()
        .unwrap_or_default()
        .join("target")
        .join("lueying-panel-selftest.txt");
    let _ = std::fs::create_dir_all("target");
    let _ = std::fs::remove_file(&report);
    let output = std::process::Command::new(&exe)
        .arg("--panel-selftest")
        .arg(&report)
        .output()
        .map_err(|e| format!("启动命令面板自检失败: {e}"))?;
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    if !output.status.success() {
        return Err(format!("命令面板自检失败: {text}"));
    }
    if !text.contains("结果:通过") {
        return Err(format!("命令面板自检没有结论: {text}"));
    }
    Ok(format!("## 命令面板自检\n\n{text}\n"))
}

fn server_log() -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_default()
        .join("target")
        .join("win-e2e-server.log")
}

/// UIA 回退的调试痕迹;server 是 GUI 子系统进程,stdout 重定向靠不住,
/// 统一写共享 TEMP 下的追加日志。
fn uia_log() -> std::path::PathBuf {
    std::env::temp_dir().join("lueying-uia.log")
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

fn install_ui_sink(
    thread_mgr: &ITfThreadMgr,
) -> Result<(ITfSource, u32, Rc<RefCell<UiEvents>>), String> {
    let source: ITfSource = thread_mgr
        .cast()
        .map_err(|e| format!("cast ITfSource: {e}"))?;
    let events = Rc::new(RefCell::new(UiEvents::default()));
    let sink: ITfUIElementSink = UiSink {
        events: Rc::clone(&events),
    }
    .into();
    let unknown: windows::core::IUnknown = sink.into();
    let cookie = unsafe { source.AdviseSink(&ITfUIElementSink::IID, &unknown) }
        .map_err(|e| format!("AdviseSink(ITfUIElementSink): {e}"))?;
    Ok((source, cookie, events))
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
        std::env::set_var("LUEYING_PIPE_SUFFIX", &suffix);
        std::env::set_var("LUEYING_TSF_LOG", &log);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("无父目录")?.to_path_buf();
    let dll = dir.join("lueying_tsf.dll");
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
    // 官方 shot-ocr 扩展装进隔离的 APPDATA;发布包带的是同一份文件。
    install_shot_ocr(&data)?;
    // TSF 在断线时可能懒拉起 Server;测试进程本身也指向隔离目录,
    // 避免清理阶段复活的 Server 读到 runner 的真实配置。
    unsafe {
        std::env::set_var("LOCALAPPDATA", &data);
        std::env::set_var("APPDATA", &data);
    }
    let server_out = server_log();
    let _ = std::fs::remove_file(&server_out);
    let _ = std::fs::remove_file(uia_log());
    let out = std::fs::File::create(&server_out).map_err(|e| format!("创建 Server 日志: {e}"))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("复制 Server 日志句柄: {e}"))?;
    let mut server = match std::process::Command::new(dir.join("lueying-server.exe"))
        .env("LOCALAPPDATA", &data)
        .env("APPDATA", &data)
        // CI 没有交互桌面,不建候选窗,只保留管道服务。
        .env("LUEYING_SERVER_NO_UI", "1")
        // D2D 渲染管线自检:WARP 离屏出帧、读回像素,结果写日志。
        .env("LUEYING_D2D_SELFTEST", "1")
        // UIA 回退的运行痕迹默认静默;E2E 要断言这条链路真的在跑。
        .env("LUEYING_DEBUG_UIA", "1")
        // 无候选窗时也起扩展宿主:官方 shot-ocr 的命令热键要靠它。
        .env("LUEYING_EXT_HOST", "1")
        // CI 镜像不一定有 OCR 语言包;识别换成固定文本,截图与上屏仍是真实现。
        .env("LUEYING_FAKE_OCR", "1")
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::from(err))
        .spawn()
    {
        Ok(child) => Some(child),
        Err(error) => return Err(format!("启动 Server {}: {error}", dir.display())),
    };
    // 给管道建立一点时间,TSF 侧自身还有懒重试兜底。Server 缺 DLL 时
    // spawn 可能成功但进程立刻退出,先把退出码和日志留下来。
    std::thread::sleep(Duration::from_millis(600));
    if let Some(child) = server.as_mut()
        && let Ok(Some(status)) = child.try_wait()
    {
        let detail = tail_of(Some(server_out));
        return Err(format!("Server 提前退出 {status}: {detail}"));
    }

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
            profiles_api.GetActiveLanguageProfile(&CLSID_IME, &mut active_lang, &mut active_guid)
        } {
            Ok(()) => {
                println!("GetActiveLanguageProfile: lang=0x{active_lang:04x} guid={active_guid:?}")
            }
            Err(e) => println!("GetActiveLanguageProfile: {e}"),
        }
        // 激活 profile 到服务实例挂上焦点是异步的;给 TSF 一点消息泵时间。
        pump_for(Duration::from_millis(400));
        report_tsf_focus();
        let touch_note = check_touch_layout(&thread_mgr)?;
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

        // 4. 中文:注入「nihao+空格」。先挂 UIElementSink,确认候选
        //    确实经 TSF 暴露给宿主/读屏软件。
        let (ui_source, ui_cookie, ui_events) = install_ui_sink(&thread_mgr)?;
        type_text("nihao")?;
        pump_for(Duration::from_millis(200));
        // 布局监听必须挂上,否则宿主滚动后候选窗会停在旧位置。
        if !std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("登记布局监听")
        {
            return Err("TSF 日志里没有布局监听登记".into());
        }
        let ui_note = check_candidate_ui(&thread_mgr, &ui_events)?;
        // UIA 左邻文本回退:开头打字时左邻为空,TSF 读不到文本必然触发
        // 回退查询。Server 受理、任务入队、读取结果都写进共享 TEMP 的追加
        // 日志(server 是 GUI 子系统进程,stdout 重定向靠不住);3 秒内没
        // 痕迹说明回退链路断了,线程级失败标记直接判失败。
        let mut uia_seen = false;
        let uia_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < uia_deadline {
            if std::fs::read_to_string(uia_log())
                .unwrap_or_default()
                .contains("UIA:")
            {
                uia_seen = true;
                break;
            }
            pump_for(Duration::from_millis(100));
        }
        if !uia_seen {
            let tail = std::fs::read_to_string(uia_log()).unwrap_or_default();
            return Err(format!("3 秒内没有 UIA 回退痕迹,临时日志尾:\n{tail}"));
        }
        let uia_log_text = std::fs::read_to_string(uia_log()).unwrap_or_default();
        for marker in [
            "UIA 创建失败",
            "UIA 线程启动失败",
            "UIA 线程 COM 初始化失败",
            "UIA 读取 panic",
        ] {
            if uia_log_text.contains(marker) {
                return Err(format!("{marker}"));
            }
        }
        tap(0x20)?; // 空格
        pump_for(Duration::from_millis(600));
        if !ui_events.borrow().ended {
            return Err("候选 UI 元素在组合结束后没有 EndUIElement".into());
        }

        // 4b. 保留键 Ctrl+.:切换中文标点并持久化到 Server 的 config.toml。
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
        // lueying-e2e 里卡;这里只抓 TSF/管道/编辑会话导致的卡死或秒级回归。
        let latency = commit_latency(edit, &thread_mgr)?;
        // 7. 宿主原样落定(搜索框 Enter):按当前显示文本上屏,不补全剩余
        //    拼音,也不改选候选;组合与 UI 元素都必须结束。
        let finalize_note = check_finalize(edit, &thread_mgr, &ui_events)?;
        // 组合里 Ctrl+Backspace 由输入法按音节删除,不能漏给宿主去改文档。
        let segment_note = check_segment_edit(edit, &thread_mgr)?;
        let fullwidth_note = check_fullwidth(edit)?;
        // D2D 管线自检结论在启动日志里;读不到或失败都让整轮红。
        let d2d_note = check_d2d_selftest()?;
        // 运行期扩展自检:Wasm guest 在 Windows 上加载、派发、隐私拦截。
        let ext_note = check_ext_selftest()?;
        // 设置窗自检:Slint 组件实例化,页面模型吃得进、关键控件在位。
        let settings_note = settings_selftest(&dir)?;
        // 命令面板自检:独立进程建可获焦窗口、拼音搜索、执行与焦点归还。
        let panel_note = panel_selftest(&dir)?;
        // 管道授权门:本进程不是工具进程,特权请求被拒、只读放行。
        let gate_note = pipe_gate::check()?;
        // 面板自检把前台还给了它自己的宿主;后面的按键要回到 RichEdit。
        unsafe {
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(edit));
        }
        pump_for(Duration::from_millis(300));
        // 每键往返与内存基线放在所有文本断言之后:延迟测试会清空编辑框,
        // 放在中间会破坏后续检查依赖的上文(原样落定靠上一轮留下的「你好」)。
        let keys = key_latency(edit, &log)?;
        // 官方 shot-ocr:热键 → 截图 → 识别 → 上屏。识别是固定文本,
        // 截图与上屏走真实现;放最后,它会清空编辑框并再抢一次焦点。
        let shot_note = check_shot_ocr(edit, hwnd)?;
        // 运行指标:shot-ocr 已加载,快照里要有它的派发计数。
        let metrics_note = crate::ext_metrics::check(&data)?;
        let memory = server_memory(server.as_ref().map_or(0, std::process::Child::id))?;
        // 扩展平台验证放最后:它会抢焦点建自己的窗口,后面的检查都已完成。
        let verify = crate::verify::section();
        let _ = unsafe { ui_source.UnadviseSink(ui_cookie) };
        Ok((
            text,
            format!(
                "{latency}\n\n{ui_note}\n\n{touch_note}\n\n{finalize_note}\n\n{segment_note}\n\n{fullwidth_note}\n\n{d2d_note}\n\n{ext_note}\n\n{settings_note}\n\n{panel_note}\n\n{gate_note}\n\n{keys}\n\n{shot_note}\n\n{metrics_note}\n\n{memory}\n\n{verify}"
            ),
        ))
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
