//! 安装到 `%ProgramFiles%\lueying`(管理员可写,避免机器级 COM 指向
//! 用户可替换的 DLL);自启动与卸载项写当前用户 HKCU。文件复制与 TSF 注册
//! 由提权子进程完成,UAC 使用其他管理员账号时自启动仍落在原用户名下。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::run();
    #[cfg(not(windows))]
    println!("lueying-install 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::io::Write;
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use lueying_protocol::{Request, pipe_name};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize, IPersistFile,
    };
    use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
    use windows::Win32::UI::Shell::{
        FOLDERID_Programs, IShellLinkW, IsUserAnAdmin, KF_FLAG_DEFAULT, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW,
    };
    use windows::Win32::UI::TextServices::{
        CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfileMgr, ITfInputProcessorProfiles,
        TF_IPPMF_DISABLEPROFILE, TF_IPPMF_FORSESSION, TF_PROFILETYPE_INPUTPROCESSOR,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW, SW_SHOWNORMAL,
    };
    use windows::core::{GUID, HSTRING, Interface, PCWSTR};

    const APP: &str = "掠影";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Lueying";
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Lueying";
    // 改名前的注册项:升级安装时清掉,避免自启动指向旧目录。
    const LEGACY_RUN_VALUE: &str = "PinyinIme";
    const LEGACY_UNINSTALL_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\PinyinIme";
    const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    /// 与 `crates/tsf/src/lib.rs` 的三处身份一致;安装器只负责把已注册的 profile
    /// 加进当前用户的输入法列表。
    const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
    const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
    const LANGID_ZH_CN: u16 = 0x0804;
    /// 提权子进程最长等待时间。更新时用户可能先看到 UAC 提示再点确认,
    /// 60 秒容易在慢机器上误报失败。
    const ELEVATED_TIMEOUT_MS: u32 = 180_000;

    pub(super) fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let quiet = args.iter().any(|a| a == "--quiet");
        let admin = unsafe { IsUserAnAdmin() }.as_bool();

        // 提权子进程只做文件与注册表动作,不碰 HKCU。
        if let Some(at) = args.iter().position(|a| a == "--install-files") {
            if !admin {
                fail(quiet, "安装文件需要管理员权限");
            }
            let (Some(source), Some(target)) = (args.get(at + 1), args.get(at + 2)) else {
                fail(quiet, "安装参数不完整");
            };
            if let Err(error) = install_files(Path::new(source), Path::new(target)) {
                fail(quiet, &error);
            }
            std::process::exit(0);
        }
        if let Some(dir) = value_after(&args, "--unregister") {
            if !admin {
                fail(quiet, "注销输入法需要管理员权限");
            }
            let target = Path::new(&dir);
            unregister(target);
            cleanup_files(target);
            std::process::exit(0);
        }

        let target = install_dir();
        if args.iter().any(|a| a == "--uninstall") {
            if let Err(error) = uninstall_user_state() {
                fail(quiet, &error);
            }
            let ok = if admin {
                unregister(&target);
                cleanup_files(&target);
                true
            } else {
                elevate(
                    &["--unregister".into(), target.display().to_string()],
                    quiet,
                )
            };
            if !ok {
                fail(quiet, "注销输入法失败(注册表未清理)");
            }
            if !quiet {
                message(APP, "已卸载。", false);
            }
            return;
        }

        let source = match current_dir() {
            Ok(source) => source,
            Err(error) => fail(quiet, &error),
        };
        if admin {
            if let Err(error) = install_files(&source, &target) {
                fail(quiet, &error);
            }
        } else if !elevate(
            &[
                "--install-files".into(),
                source.display().to_string(),
                target.display().to_string(),
            ],
            quiet,
        ) {
            fail(quiet, "安装文件失败,请重试或查看是否拒绝了管理员提示");
        }
        let listed = match install_user_state(&target) {
            Ok(listed) => listed,
            Err(error) => fail(quiet, &error),
        };
        // 立即拉起常驻服务;单实例互斥保证不会重复。
        let _ = std::process::Command::new(target.join("lueying-server.exe"))
            .current_dir(&target)
            .spawn();
        if !quiet {
            let hint = if listed {
                "按 Win+空格 切换到「掠影」。"
            } else {
                "未能加入输入法列表。请先在 设置 → 时间和语言 → 语言和区域\n\
                 添加「中文(简体)」，再运行安装器。"
            };
            message(
                APP,
                &format!("已安装到:\n{}\n\n{hint}", target.display()),
                false,
            );
        }
    }

    fn fail(quiet: bool, text: &str) -> ! {
        if !quiet {
            message(APP, text, true);
        }
        std::process::exit(1);
    }

    fn value_after(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }

    /// 提权部分:停服务、复制文件、注册 x64/x86 TSF。
    fn install_files(source: &Path, target: &Path) -> Result<(), String> {
        stop_server();
        let version = version(source);
        copy_payload(source, target)?;
        copy_dll(source, target, false, &dll_name(&version))?;
        // 32 位 DLL 是可选文件:旧发布目录没有时仍能安装 x64。
        copy_dll(source, target, true, &dll_name32(&version))?;
        prune_old_dlls(target, &dll_name(&version), &dll_name32(&version));
        register(target)
    }

    /// 当前用户部分:输入法列表、自启动、卸载项、快捷方式。
    /// 返回 profile 是否已加入输入法列表。
    fn install_user_state(target: &Path) -> Result<bool, String> {
        // DllRegisterServer 已按默认启用注册 profile;这里再显式启用一次,
        // 覆盖旧版注册或用户曾关闭后升级的状态,装完 Win+空格 就能看到「掠影」。
        // 必须在当前用户进程里调用,不能放进提权子进程。加入失败(例如系统
        // 还没装中文语言)不中止安装,由调用方提示下一步。
        let listed = set_profile_enabled(true).is_ok();
        set_run_key(&target.join("lueying-server.exe"))?;
        register_uninstall(target)?;
        let settings = target.join("lueying-settings.exe");
        let _ = shortcut(
            &start_menu().join("掠影设置.lnk"),
            &settings,
            "",
            "掠影设置",
        );
        let _ = shortcut(
            &start_menu().join("卸载掠影.lnk"),
            &target.join("lueying-install.exe"),
            "--uninstall",
            "卸载掠影",
        );
        // 改名前的快捷方式与注册项不留在系统里。
        for name in ["拼音输入法设置.lnk", "卸载拼音输入法.lnk"] {
            let _ = std::fs::remove_file(start_menu().join(name));
        }
        remove_legacy_registry();
        Ok(listed)
    }

    fn uninstall_user_state() -> Result<(), String> {
        stop_server();
        // 先把 profile 从当前用户的输入法列表移除,再提权删除文件;否则列表里
        // 会留下一个指向已删 DLL 的「掠影」。
        let _ = set_profile_enabled(false);
        remove_registry();
        remove_shortcuts();
        // 文件可能已被用户手工删除;注册项仍要清干净,不能因此中止卸载。
        Ok(())
    }

    /// 把已注册的 TSF profile 加进/移出当前用户的输入法列表。
    fn set_profile_enabled(enabled: bool) -> Result<(), String> {
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

    fn register(target: &Path) -> Result<(), String> {
        unregister(target);
        let version = version(target);
        let dll = target.join(dll_name(&version));
        let dll = if dll.exists() {
            dll
        } else {
            newest_dll(target, false).ok_or_else(|| "安装目录里没有 x64 TSF DLL".to_string())?
        };
        regsvr32(&dll, false, false)?;
        let dll32 = target.join(dll_name32(&version));
        let dll32 = if dll32.exists() {
            Some(dll32)
        } else {
            newest_dll(target, true)
        };
        if let Some(dll32) = dll32 {
            regsvr32(&dll32, true, false)?;
        }
        Ok(())
    }

    fn unregister(target: &Path) {
        let Ok(entries) = std::fs::read_dir(target) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("lueying_tsf") && name.ends_with(".dll") {
                let wow64 = name.starts_with("lueying_tsf32");
                let _ = regsvr32(&entry.path(), wow64, true);
            }
        }
    }

    /// 升级后清掉旧版本 DLL;仍被应用加载的文件删不掉,留给下次安装。
    fn prune_old_dlls(target: &Path, keep64: &str, keep32: &str) {
        let Ok(entries) = std::fs::read_dir(target) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            if name.eq_ignore_ascii_case(keep64)
                || name.eq_ignore_ascii_case(keep32)
                || !lower.starts_with("lueying_tsf")
                || !lower.ends_with(".dll")
            {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
    }

    /// 安装包里找 x64/x86 DLL:优先无版本名的构建产物,其次版本化文件。
    fn source_dll(source: &Path, want32: bool) -> Option<PathBuf> {
        let plain = source.join(if want32 {
            "lueying_tsf32.dll"
        } else {
            "lueying_tsf.dll"
        });
        plain
            .exists()
            .then_some(plain)
            .or_else(|| newest_dll(source, want32))
    }

    fn copy_dll(source: &Path, target: &Path, want32: bool, name: &str) -> Result<(), String> {
        let Some(source) = source_dll(source, want32) else {
            if want32 {
                return Ok(());
            }
            return Err("安装包里没有 x64 TSF DLL".into());
        };
        let target = target.join(name);
        if source == target {
            return Ok(());
        }
        std::fs::copy(&source, &target)
            .map(|_| ())
            .map_err(|e| format!("复制 TSF DLL {}: {e}", source.display()))
    }

    fn newest_dll(target: &Path, want32: bool) -> Option<PathBuf> {
        std::fs::read_dir(target)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let is_dll = path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"));
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                is_dll
                    && if want32 {
                        name.starts_with("lueying_tsf32")
                    } else {
                        name.starts_with("lueying_tsf") && !name.starts_with("lueying_tsf32")
                    }
            })
            .max_by_key(|path| {
                std::fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .ok()
            })
    }

    /// `regsvr32 /s` 调用对应位数的注册器;x86 DLL 必须走 SysWOW64,
    /// 64 位进程不能 LoadLibrary 32 位 DLL。
    fn regsvr32(dll: &Path, wow64: bool, unregister: bool) -> Result<(), String> {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let exe = root
            .join(if wow64 { "SysWOW64" } else { "System32" })
            .join("regsvr32.exe");
        if !exe.exists() {
            return Err(format!("找不到 {}", exe.display()));
        }
        let mut command = Command::new(exe);
        command.arg("/s");
        if unregister {
            command.arg("/u");
        }
        command.arg(dll);
        let status = command
            .status()
            .map_err(|e| format!("启动 regsvr32: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "regsvr32 {} 返回 {:?}",
                dll.display(),
                status.code()
            ))
        }
    }

    /// 用 runas 拉起本程序并等待退出;返回子进程是否成功。
    fn elevate(args: &[String], quiet: bool) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let verb = HSTRING::from("runas");
        let file = HSTRING::from(exe.to_string_lossy().to_string());
        let params = HSTRING::from(
            args.iter()
                .map(|a| quote_arg(a))
                .chain(quiet.then(|| "--quiet".to_string()))
                .collect::<Vec<_>>()
                .join(" "),
        );
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if unsafe { ShellExecuteExW(&mut info) }.is_err() || info.hProcess.is_invalid() {
            return false;
        }
        let waited = unsafe { WaitForSingleObject(info.hProcess, ELEVATED_TIMEOUT_MS) };
        let mut code = 1u32;
        let _ = unsafe { GetExitCodeProcess(info.hProcess, &mut code) };
        unsafe {
            let _ = CloseHandle(info.hProcess);
        }
        waited == WAIT_OBJECT_0 && code == 0
    }

    fn quote_arg(arg: &str) -> String {
        if arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
            format!("\"{}\"", arg.replace('"', "\\\""))
        } else {
            arg.to_string()
        }
    }

    fn current_dir() -> Result<PathBuf, String> {
        std::env::current_exe()
            .map_err(|e| format!("定位安装包: {e}"))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "安装包路径异常".into())
    }

    fn install_dir() -> PathBuf {
        // 测试用隔离目录;正式安装走 Program Files(管理员可写),避免机器级
        // COM 指向用户可替换的 DLL。
        if let Some(dir) = std::env::var_os("LUEYING_INSTALL_DIR") {
            return PathBuf::from(dir);
        }
        if let Some(dir) = std::env::var_os("ProgramFiles") {
            return PathBuf::from(dir).join("lueying");
        }
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Programs")
            .join("lueying")
    }

    fn appdata() -> PathBuf {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn start_menu() -> PathBuf {
        // 企业环境可能重定向开始菜单;按 Known Folder 取,不用拼 APPDATA。
        if let Ok(path) = unsafe { SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None) }
        {
            let text = unsafe { path.to_string() }.ok();
            unsafe { CoTaskMemFree(Some(path.0 as *const core::ffi::c_void)) };
            if let Some(text) = text {
                return PathBuf::from(text);
            }
        }
        appdata()
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
    }

    fn copy_payload(source: &Path, target: &Path) -> Result<(), String> {
        if source == target {
            return Ok(());
        }
        Self::copy_tree(source, target, true)
    }

    /// 递归复制发布目录。`licenses/` 这类子目录要随二进制一起进安装目录,
    /// 否则第三方许可在分发时丢失。顶层跳过 TSF DLL,它们单独按版本名复制。
    fn copy_tree(source: &Path, target: &Path, top: bool) -> Result<(), String> {
        std::fs::create_dir_all(target).map_err(|e| format!("创建 {}: {e}", target.display()))?;
        let entries = std::fs::read_dir(source)
            .map_err(|e| format!("读取安装包目录 {}: {e}", source.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("读取安装包文件: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                Self::copy_tree(&path, &target.join(entry.file_name()), false)?;
                continue;
            }
            if !path.is_file() {
                continue;
            }
            // TSF DLL 以版本号命名单独复制,避免覆盖正在被应用加载的旧文件。
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if top && name.starts_with("lueying_tsf") && name.ends_with(".dll") {
                continue;
            }
            let destination = target.join(entry.file_name());
            std::fs::copy(&path, &destination)
                .map_err(|e| format!("复制 {}: {e}", path.display()))?;
        }
        Ok(())
    }

    fn stop_server() {
        let mut pid = 0u32;
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name())
        else {
            return;
        };
        unsafe {
            let _ = GetNamedPipeServerProcessId(HANDLE(file.as_raw_handle()), &mut pid);
        }
        let mut line = serde_json::to_string(&Request::Shutdown).unwrap_or_default();
        line.push('\n');
        {
            let mut writer = file;
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }
        std::thread::sleep(std::time::Duration::from_millis(800));
        if pid != 0 {
            unsafe {
                if let Ok(process) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                    let _ = TerminateProcess(process, 0);
                    let _ = CloseHandle(process);
                }
            }
        }
    }

    fn set_run_key(server: &Path) -> Result<(), String> {
        let key = windows_registry::CURRENT_USER
            .create(RUN_KEY)
            .map_err(|e| format!("打开自启动注册表: {e}"))?;
        key.set_string(RUN_VALUE, format!("\"{}\"", server.display()))
            .map_err(|e| format!("写自启动: {e}"))
    }

    fn register_uninstall(target: &Path) -> Result<(), String> {
        let key = windows_registry::CURRENT_USER
            .create(UNINSTALL_KEY)
            .map_err(|e| format!("打开卸载注册表: {e}"))?;
        let installer = target.join("lueying-install.exe");
        let set = |name: &str, value: String| {
            key.set_string(name, value)
                .map_err(|e| format!("写卸载项 {name}: {e}"))
        };
        set("DisplayName", APP.to_string())?;
        set("DisplayVersion", version(target))?;
        set("Publisher", "掠影".into())?;
        set("InstallLocation", target.display().to_string())?;
        let settings = target.join("lueying-settings.exe");
        set("DisplayIcon", settings.display().to_string())?;
        set(
            "UninstallString",
            format!("\"{}\" --uninstall", installer.display()),
        )?;
        set(
            "QuietUninstallString",
            format!("\"{}\" --uninstall --quiet", installer.display()),
        )?;
        key.set_u32("NoModify", 1)
            .map_err(|e| format!("写 NoModify: {e}"))?;
        key.set_u32("NoRepair", 1)
            .map_err(|e| format!("写 NoRepair: {e}"))
    }

    fn version(target: &Path) -> String {
        std::fs::read_to_string(target.join("version.txt"))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
    }

    /// 版本号只留文件名安全字符。
    fn safe_version(version: &str) -> String {
        version
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn dll_name(version: &str) -> String {
        format!("lueying_tsf-{}.dll", safe_version(version))
    }

    fn dll_name32(version: &str) -> String {
        format!("lueying_tsf32-{}.dll", safe_version(version))
    }

    fn remove_registry() {
        let _ = windows_registry::CURRENT_USER
            .create(RUN_KEY)
            .and_then(|key| key.remove_value(RUN_VALUE));
        let _ = windows_registry::CURRENT_USER.remove_tree(UNINSTALL_KEY);
        remove_legacy_registry();
    }

    fn remove_legacy_registry() {
        let _ = windows_registry::CURRENT_USER
            .create(RUN_KEY)
            .and_then(|key| key.remove_value(LEGACY_RUN_VALUE));
        let _ = windows_registry::CURRENT_USER.remove_tree(LEGACY_UNINSTALL_KEY);
    }

    fn remove_shortcuts() {
        let dir = start_menu();
        for name in ["掠影设置.lnk", "卸载掠影.lnk"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }

    fn cleanup_files(target: &Path) {
        if !target.exists() {
            return;
        }
        let self_inside = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .is_some_and(|dir| dir == target);
        if !self_inside && std::fs::remove_dir_all(target).is_ok() {
            return;
        }
        // 安装器自己或已加载的 DLL 占用文件;退出后由 cmd 再删。
        let _ = std::process::Command::new("cmd")
            .args([
                "/C",
                "ping",
                "-n",
                "2",
                "127.0.0.1",
                ">nul",
                "&",
                "rmdir",
                "/s",
                "/q",
            ])
            .arg(target)
            .spawn();
    }

    fn shortcut(path: &Path, target: &Path, arguments: &str, description: &str) -> Option<()> {
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let result = (|| -> windows::core::Result<()> {
            let link: IShellLinkW =
                unsafe { CoCreateInstance(&CLSID_SHELL_LINK, None, CLSCTX_INPROC_SERVER)? };
            unsafe {
                link.SetPath(&HSTRING::from(target.to_string_lossy().to_string()))?;
                if let Some(dir) = target.parent() {
                    link.SetWorkingDirectory(&HSTRING::from(dir.to_string_lossy().to_string()))?;
                }
                link.SetDescription(&HSTRING::from(description))?;
                link.SetIconLocation(&HSTRING::from(target.to_string_lossy().to_string()), 0)?;
                link.SetArguments(&HSTRING::from(arguments))?;
                let file: IPersistFile = link.cast()?;
                file.Save(&HSTRING::from(path.to_string_lossy().to_string()), true)?;
            }
            Ok(())
        })();
        if initialized {
            unsafe { CoUninitialize() };
        }
        result.ok()
    }

    fn message(title: &str, text: &str, error: bool) {
        let style = if error {
            MB_ICONERROR
        } else {
            MB_ICONINFORMATION
        };
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(text),
                &HSTRING::from(title),
                MB_OK | style,
            );
        }
    }
}
