//! 当前用户辅助程序。机器级安装(复制文件、注册 TSF、卸载项)由 Inno Setup
//! 完成;这里只处理必须落在发起安装的用户名下的状态:输入法列表、自启动项,
//! 以及升级/卸载前停止常驻服务。
//!
//! `--install` 由安装包的 `[Run] runasoriginaluser` 调用;`--uninstall` 由卸载器
//! 提权调用。程序没有控制台输出,失败信息写入 `LUEYING_INSTALL_TRACE`。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::run();
    #[cfg(not(windows))]
    println!("lueying-user 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use lueying_protocol::{Request, pipe_name};
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
    use windows::Win32::UI::TextServices::{
        CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfileMgr, ITfInputProcessorProfiles,
        TF_IPPMF_DISABLEPROFILE, TF_IPPMF_FORSESSION, TF_PROFILETYPE_INPUTPROCESSOR,
    };
    use windows::core::{GUID, HSTRING, Interface, PCWSTR};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Lueying";
    const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
    const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
    const LANGID_ZH_CN: u16 = 0x0804;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// 安装包与更新器都看不到控制台输出;设置 `LUEYING_INSTALL_TRACE` 后把
    /// 阶段追加到该文件,CI 卡住时仍能定位。
    fn trace(text: &str) {
        let Some(path) = std::env::var_os("LUEYING_INSTALL_TRACE") else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{text}");
        }
    }

    pub(super) fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("--install") => {
                if let Err(error) = install() {
                    trace(&format!("install: 失败: {error}"));
                    std::process::exit(1);
                }
            }
            Some("--uninstall") => uninstall(),
            Some("--stop-server") => {
                stop_server();
                trace("stop_server: done");
            }
            Some("--prune") => prune(&args),
            _ => {
                trace(&format!("helper: 未知参数 {args:?}"));
                std::process::exit(2);
            }
        }
    }

    /// 当前用户状态。输入法列表加入失败不算安装失败:系统可能还没装中文语言,
    /// 提示由设置窗负责;自启动写不进去才返回错误。
    fn install() -> Result<(), String> {
        let target = app_dir()?;
        trace(&format!("install: {}", target.display()));
        let listed = set_profile_enabled(true).is_ok();
        trace(&format!("install: profile listed={listed}"));
        set_run_key(&target.join("lueying-server.exe"))?;
        // 扩展不再由安装器自动复制:设置窗的「官方扩展」按需安装并走同意流程。
        if let Err(error) =
            lueying_protocol::spawn_detached(&target.join("lueying-server.exe"), Some(&target))
        {
            trace(&format!("install: spawn server failed: {error}"));
        }
        trace("install: done");
        Ok(())
    }

    fn uninstall() {
        trace("uninstall: stop_server");
        stop_server();
        trace("uninstall: profile");
        let _ = set_profile_enabled(false);
        trace("uninstall: registry");
        remove_registry();
        trace("uninstall: done");
    }

    /// 删除旧版本 DLL。被宿主加载的文件删不掉,留给下次安装;失败不报错。
    fn prune(args: &[String]) {
        let Some(keep) = value_after(args, "--keep") else {
            trace("prune: 缺少 --keep");
            return;
        };
        let Ok(target) = app_dir() else {
            return;
        };
        prune_old_dlls(&target, &format!("lueying_tsf-{keep}.dll"));
    }

    fn prune_old_dlls(target: &Path, keep: &str) {
        let Ok(entries) = std::fs::read_dir(target) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if name.eq_ignore_ascii_case(keep)
                || !lower.starts_with("lueying_tsf")
                || !lower.ends_with(".dll")
            {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }

    fn value_after(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }

    fn app_dir() -> Result<PathBuf, String> {
        std::env::current_exe()
            .map_err(|e| format!("定位程序目录: {e}"))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "程序路径异常".into())
    }

    /// 把已注册的 TSF profile 加进/移出当前用户的输入法列表。
    ///
    /// profile 管理器在无桌面/CI 会话里可能一直不返回;这不是安装的硬前提,
    /// 放到独立线程里限时等待,超时就当失败继续,别把安装器拖住。
    fn set_profile_enabled(enabled: bool) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(set_profile_enabled_blocking(enabled));
        });
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| "输入法列表操作超过 3 秒".to_string())?
    }

    fn set_profile_enabled_blocking(enabled: bool) -> Result<(), String> {
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let result = (|| -> windows::core::Result<()> {
            let profiles: ITfInputProcessorProfiles = unsafe {
                CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?
            };
            if enabled {
                return unsafe {
                    profiles.EnableLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE, true)
                };
            }
            // Win10/11 上 DeactivateProfile 比 EnableLanguageProfile(false) 可靠,
            // 老接口作兜底。
            let deactivated = match profiles.cast::<ITfInputProcessorProfileMgr>() {
                Ok(manager) => unsafe {
                    manager.DeactivateProfile(
                        TF_PROFILETYPE_INPUTPROCESSOR,
                        LANGID_ZH_CN,
                        &CLSID_IME,
                        &GUID_PROFILE,
                        HKL::default(),
                        TF_IPPMF_FORSESSION | TF_IPPMF_DISABLEPROFILE,
                    )
                }
                .is_ok(),
                Err(_) => false,
            };
            if deactivated {
                Ok(())
            } else {
                unsafe {
                    profiles.EnableLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE, false)
                }
            }
        })();
        if initialized {
            unsafe { CoUninitialize() };
        }
        result.map_err(|e| {
            format!(
                "{}输入法列表失败: {e}",
                if enabled { "加入" } else { "移出" }
            )
        })
    }

    /// 先按当前用户的管道名请服务退出,再按进程名兜底。
    ///
    /// 管理员代装时,提权进程的管道名与被安装用户不同,连不上原用户的服务;
    /// 而升级必须替换 lueying-server.exe。TSF 会在下次输入时按需重新拉起服务,
    /// 所以这里接受停止本机所有 lueying-server 进程。
    fn stop_server() {
        trace("stop_server: wait pipe");
        let name = HSTRING::from(pipe_name());
        if unsafe { WaitNamedPipeW(&name, 200) }.as_bool() {
            if let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe_name())
            {
                let mut pid = 0u32;
                unsafe {
                    let _ = GetNamedPipeServerProcessId(HANDLE(file.as_raw_handle()), &mut pid);
                }
                if let Ok(frame) = lueying_protocol::encode(&Request::Shutdown) {
                    let mut writer = file;
                    let _ = writer.write_all(&frame);
                    let _ = writer.flush();
                }
                trace("stop_server: terminate pipe owner");
                std::thread::sleep(std::time::Duration::from_millis(800));
                if pid != 0 {
                    terminate(pid);
                }
            } else {
                trace("stop_server: open failed");
            }
        } else {
            trace("stop_server: no server");
        }
        trace("stop_server: taskkill");
        let mut command = Command::new(system32("taskkill.exe"));
        command
            .args(["/F", "/IM", "lueying-server.exe"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = command.status();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    fn terminate(pid: u32) {
        unsafe {
            if let Ok(process) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(process, 0);
                let _ = CloseHandle(process);
            }
        }
    }

    fn system32(name: &str) -> PathBuf {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join(name)
    }

    fn set_run_key(server: &Path) -> Result<(), String> {
        let key =
            create_key(HKEY_CURRENT_USER, RUN_KEY).map_err(|e| format!("打开自启动注册表: {e}"))?;
        set_string(key, RUN_VALUE, &format!("\"{}\"", server.display()))
            .map_err(|e| format!("写自启动: {e}"))?;
        let _ = unsafe { RegCloseKey(key) };
        Ok(())
    }

    fn remove_registry() {
        delete_value(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
    }

    fn create_key(parent: HKEY, subkey: &str) -> Result<HKEY, String> {
        let subkey = HSTRING::from(subkey);
        let mut key = HKEY::default();
        let err = unsafe {
            RegCreateKeyExW(
                parent,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if err == ERROR_SUCCESS {
            Ok(key)
        } else {
            Err(format!("注册表错误 0x{:08x}", err.0))
        }
    }

    fn set_string(key: HKEY, name: &str, value: &str) -> Result<(), String> {
        let name = HSTRING::from(name);
        let data: Vec<u8> = value
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .chain([0, 0])
            .collect();
        let err = unsafe {
            RegSetValueExW(
                key,
                PCWSTR(name.as_ptr()),
                Some(0),
                REG_SZ,
                Some(data.as_slice()),
            )
        };
        if err == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("注册表错误 0x{:08x}", err.0))
        }
    }

    /// 清理语义:键打不开就当没有,不报错。
    fn delete_value(parent: HKEY, subkey: &str, name: &str) {
        let subkey = HSTRING::from(subkey);
        let name = HSTRING::from(name);
        let mut key = HKEY::default();
        let opened = unsafe {
            RegOpenKeyExW(
                parent,
                PCWSTR(subkey.as_ptr()),
                Some(0),
                KEY_SET_VALUE,
                &mut key,
            )
        };
        if opened == ERROR_SUCCESS {
            let _ = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
            let _ = unsafe { RegCloseKey(key) };
        }
    }
}
