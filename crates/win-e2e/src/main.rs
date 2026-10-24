//! Windows 全链路 E2E:进程内直调 DllRegisterServer 注册 DLL → 激活本输入法 →
//! 自建 EDIT 宿主窗口 → SendInput 注入「nihao+空格」 → 读回文本断言「你好」 → 清理注册。
//! 产物 target/win-e2e-report.md。CI 首期 continue-on-error 观察,稳定后转门禁。

fn main() {
    #[cfg(windows)]
    {
        imp::run();
    }
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::LibraryLoader::{FreeLibrary, GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HKL, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SetFocus,
        VIRTUAL_KEY,
    };
    use windows::Win32::UI::TextServices::{
        CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfileMgr, TF_INPUTPROCESSORPROFILE,
        TF_IPPMF_ENABLEPROFILE, TF_PROFILETYPE_INPUTPROCESSOR,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetWindowTextW, MSG, PM_REMOVE,
        PeekMessageW, RegisterClassW, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
        TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW, WS_CHILD, WS_EX_CLIENTEDGE,
        WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
    };
    use windows::core::{GUID, HSTRING, Interface, PCSTR};

    const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
    const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
    const LANGID_ZH_CN: u16 = 0x0804;

    pub(super) fn run() {
        let report = match real_run() {
            Ok(text) => format!("# Windows E2E\n\nPASS\n\nEDIT 文本:`{text}`\n"),
            Err(e) => format!("# Windows E2E\n\nFAIL\n\n{e}\n"),
        };
        println!("{report}");
        let _ = std::fs::create_dir_all("target");
        let _ = std::fs::write("target/win-e2e-report.md", &report);
        if !report.contains("PASS") {
            std::process::exit(1);
        }
    }

    fn real_run() -> Result<String, String> {
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
        let reg_fn = unsafe { GetProcAddress(dll_h, PCSTR::from_raw(c"DllRegisterServer".as_ptr())) }
            .map(|f| unsafe { std::mem::transmute::<_, DllServer>(f) })
            .ok_or("DLL 未导出 DllRegisterServer")?;
        let hr = unsafe { reg_fn() };
        println!("DllRegisterServer: 0x{:08X}", hr.0 as u32);
        if hr.is_err() {
            return Err(format!("DllRegisterServer: 0x{:08X}", hr.0 as u32));
        }

        // 拉起 server(主词典缺失会退回内置迷你词库,E2E 断言用它)
        let mut server = std::process::Command::new(dir.join("pinyin-server.exe"))
            .spawn()
            .ok();
        // 给管道建立一点时间,TSF 侧自身还有懒重试兜底。
        std::thread::sleep(Duration::from_millis(600));

        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;

        let result = (|| -> Result<String, String> {
            // 2. 激活本输入法(本进程)
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
                    "GetProfile: 可见 enabled={} byDefault={}",
                    prof.fEnabled.as_bool(),
                    prof.fByDefault.as_bool()
                ),
                Err(e) => println!("GetProfile: {e}"),
            }
            if let Ok(enum_) = unsafe { mgr.EnumProfiles(LANGID_ZH_CN) } {
                let mut item = TF_INPUTPROCESSORPROFILE::default();
                let mut got = 0u32;
                let mut n = 0usize;
                while unsafe { enum_.Next(1, &mut item, &mut got) }.is_ok() && got > 0 && n < 64 {
                    n += 1;
                }
                println!("EnumProfiles(0x{LANGID_ZH_CN:04x}): {n} 个");
            }

            // 2. 激活本输入法(本进程);先默认 flags,失败换 ENABLEPROFILE
            let act = unsafe {
                mgr.ActivateProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    LANGID_ZH_CN,
                    &CLSID_IME,
                    &GUID_PROFILE,
                    HKL::default(),
                    0,
                )
            };
            let act = match act {
                Ok(()) => Ok(()),
                Err(e0) => {
                    println!("ActivateProfile(flags=0): {e0},重试 ENABLEPROFILE");
                    unsafe {
                        mgr.ActivateProfile(
                            TF_PROFILETYPE_INPUTPROCESSOR,
                            LANGID_ZH_CN,
                            &CLSID_IME,
                            &GUID_PROFILE,
                            HKL::default(),
                            TF_IPPMF_ENABLEPROFILE,
                        )
                    }
                }
            };
            act.map_err(|e| format!("ActivateProfile: {e}"))?;

            // 3. 宿主窗口 + EDIT 控件
            let (hwnd, edit) = create_host_window()?;
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(edit));
            }

            // 4. 注入按键
            for c in "nihao".chars() {
                tap(vk_of(c))?;
                pump_for(Duration::from_millis(60));
            }
            tap(0x20)?; // 空格
            pump_for(Duration::from_millis(800));

            // 5. 读回
            let mut buf = [0u16; 512];
            let n = unsafe { GetWindowTextW(edit, &mut buf) };
            let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if text.contains("你好") {
                Ok(text)
            } else {
                Err(format!("EDIT 文本 `{text}` 不含 你好"))
            }
        })();

        // 6. 清理(尽力)
        if let Some(mut child) = server.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::process::Command::new("regsvr32")
            .arg("/s")
            .arg("/u")
            .arg(&dll)
            .status();
        unsafe { FreeLibrary(dll_h) };
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

    fn create_host_window() -> Result<(HWND, HWND), String> {
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
        let edit_name: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let edit = unsafe {
            CreateWindowExW(
                WS_EX_CLIENTEDGE,
                PCWSTR(edit_name.as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
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
        .map_err(|e| format!("CreateWindowExW EDIT: {e}"))?;
        Ok((hwnd, edit))
    }

    fn vk_of(c: char) -> u16 {
        c.to_ascii_uppercase() as u16
    }

    fn tap(vk: u16) -> Result<(), String> {
        let down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: Default::default(),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent = unsafe { SendInput(&[down, up], std::mem::size_of::<INPUT>() as i32) };
        if sent != 2 {
            return Err(format!("SendInput 只发出 {sent}"));
        }
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
