//! 安装到 `%ProgramFiles%\pinyin-ime`(管理员可写,避免机器级 COM 指向
//! 用户可替换的 DLL);自启动与卸载项写当前用户 HKCU。文件复制与 TSF 注册
//! 由提权子进程完成,UAC 使用其他管理员账号时自启动仍落在原用户名下。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::run();
    #[cfg(not(windows))]
    println!("pinyin-install 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::io::Write;
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use pinyin_protocol::{Request, pipe_name};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile,
    };
    use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        IShellLinkW, IsUserAnAdmin, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW, SW_SHOWNORMAL,
    };
    use windows::core::{GUID, HSTRING, Interface, PCWSTR};

    const APP: &str = "掠影";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "PinyinIme";
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\PinyinIme";
    const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    /// 提权子进程最长等待时间。
    const ELEVATED_TIMEOUT_MS: u32 = 60_000;

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
        if let Err(error) = install_user_state(&target) {
            fail(quiet, &error);
        }
        // 立即拉起常驻服务;单实例互斥保证不会重复。
        let _ = std::process::Command::new(target.join("pinyin-server.exe"))
            .current_dir(&target)
            .spawn();
        if !quiet {
            message(
                APP,
                &format!(
                    "已安装到:\n{}\n\n按 Win+空格 切换到「掠影」。",
                    target.display()
                ),
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

    /// 当前用户部分:自启动、卸载项、快捷方式。
    fn install_user_state(target: &Path) -> Result<(), String> {
        set_run_key(&target.join("pinyin-server.exe"))?;
        register_uninstall(target)?;
        let settings = target.join("pinyin-settings.exe");
        let _ = shortcut(
            &start_menu().join("掠影设置.lnk"),
            &settings,
            "",
            "掠影设置",
        );
        let _ = shortcut(
            &start_menu().join("卸载掠影.lnk"),
            &target.join("pinyin-install.exe"),
            "--uninstall",
            "卸载掠影",
        );
        // 旧产品名的快捷方式不留在开始菜单。
        for name in ["拼音输入法设置.lnk", "卸载拼音输入法.lnk"] {
            let _ = std::fs::remove_file(start_menu().join(name));
        }
        Ok(())
    }

    fn uninstall_user_state() -> Result<(), String> {
        stop_server();
        remove_registry();
        remove_shortcuts();
        // 文件可能已被用户手工删除;注册项仍要清干净,不能因此中止卸载。
        Ok(())
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
            if name.starts_with("pinyin_tsf") && name.ends_with(".dll") {
                let wow64 = name.starts_with("pinyin_tsf32");
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
                || !lower.starts_with("pinyin_tsf")
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
            "pinyin_tsf32.dll"
        } else {
            "pinyin_tsf.dll"
        });
        plain.exists().then_some(plain).or_else(|| newest_dll(source, want32))
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
                        name.starts_with("pinyin_tsf32")
                    } else {
                        name.starts_with("pinyin_tsf") && !name.starts_with("pinyin_tsf32")
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
        if let Some(dir) = std::env::var_os("PINYIN_INSTALL_DIR") {
            return PathBuf::from(dir);
        }
        if let Some(dir) = std::env::var_os("ProgramFiles") {
            return PathBuf::from(dir).join("pinyin-ime");
        }
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Programs")
            .join("pinyin-ime")
    }

    fn appdata() -> PathBuf {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn start_menu() -> PathBuf {
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
        std::fs::create_dir_all(target).map_err(|e| format!("创建 {}: {e}", target.display()))?;
        let entries = std::fs::read_dir(source)
            .map_err(|e| format!("读取安装包目录 {}: {e}", source.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("读取安装包文件: {e}"))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // TSF DLL 以版本号命名单独复制,避免覆盖正在被应用加载的旧文件。
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with("pinyin_tsf") && name.ends_with(".dll") {
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
        let installer = target.join("pinyin-install.exe");
        let set = |name: &str, value: String| {
            key.set_string(name, value)
                .map_err(|e| format!("写卸载项 {name}: {e}"))
        };
        set("DisplayName", APP.to_string())?;
        set("DisplayVersion", version(target))?;
        set("Publisher", "掠影".into())?;
        set("InstallLocation", target.display().to_string())?;
        let settings = target.join("pinyin-settings.exe");
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
        format!("pinyin_tsf-{}.dll", safe_version(version))
    }

    fn dll_name32(version: &str) -> String {
        format!("pinyin_tsf32-{}.dll", safe_version(version))
    }

    fn remove_registry() {
        let _ = windows_registry::CURRENT_USER
            .create(RUN_KEY)
            .and_then(|key| key.remove_value(RUN_VALUE));
        let _ = windows_registry::CURRENT_USER.remove_tree(UNINSTALL_KEY);
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
