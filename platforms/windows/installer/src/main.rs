//! 当前用户辅助程序。机器级安装(复制文件、注册 TSF、卸载项)由 Inno Setup
//! 完成;uiAccess 本体的机器级签名由本程序的提权路径(`--sign-server`)完成。
//! 这里还处理必须落在发起安装的用户名下的状态:输入法列表、自启动项,
//! 以及升级/卸载前停止常驻服务。
//!
//! `--sign-server` 由安装包的 `[Run]`(提权上下文)调用;`--install` 由
//! `[Run] runasoriginaluser` 调用;`--uninstall` 由卸载器提权调用。
//! 程序没有控制台输出,失败信息写入 `LUEYING_INSTALL_TRACE`。

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
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use lueying_protocol::{Request, pipe_name};
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, FreeLibrary, HANDLE};
    use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
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
    use windows::core::{GUID, HSTRING, Interface, PCSTR, PCWSTR, w};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Lueying";
    const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
    const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
    const LANGID_ZH_CN: u16 = 0x0804;
    /// `InstallLayoutOrTip` 的字符串格式:
    /// `<LANGID>:<TIP CLSID><PROFILE GUID>`。
    const PROFILE_ID: &str =
        "0804:{8FFECD5B-DBA4-480C-9749-FECDCAA8FA17}{76F58072-AB35-4AA7-BA2D-324F994587DB}";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// 自签名代码签名证书主题;升级按主题复用,卸载按主题清理。
    const CERT_SUBJECT: &str = "CN=lueying dev";

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
            Some("--sign-server") => {
                if let Err(error) = sign_server() {
                    trace(&format!("sign_server: 失败: {error}"));
                    std::process::exit(1);
                }
                trace("sign_server: done");
            }
            Some("--uninstall") => uninstall(),
            Some("--stop-server") => {
                stop_server();
                trace("stop_server: done");
            }
            Some("--start-server") => {
                // 带 uiAccess 清单的 server 只能由普通过滤令牌拉起;本辅助
                // 程序以原用户身份运行,正是合格令牌;提权进程降级拉起会被
                // 系统以 740 拒绝。与 TSF 按需拉起走同一条 spawn_detached。
                match app_dir() {
                    Ok(dir) => {
                        let exe = dir.join("lueying-server.exe");
                        if let Err(error) = lueying_protocol::spawn_detached(&exe, None) {
                            trace(&format!("start_server: 失败: {error}"));
                        } else {
                            trace("start_server: done");
                        }
                    }
                    Err(error) => trace(&format!("start_server: {error}")),
                }
            }
            Some("--prune") => prune(),
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
        // 不在这里拉起服务:本程序可能随安装器提权运行,拉起来的服务会带管理员
        // 令牌,普通应用里的 TSF 查不到它的映像名,按假服务拒连,所有键直通。
        // 服务由 TSF 在激活/首次按键时按宿主自己的权限拉起。
        fetch_models(&target);
        trace("install: done");
        Ok(())
    }

    /// 重排模型不随安装包分发:装完在后台补齐,下完由设置窗让服务带新模型重启。
    /// 词典与搭配表随包,断网装完也能打字,模型随后补上。
    fn fetch_models(target: &Path) {
        if let Err(error) = lueying_protocol::spawn_detached_args(
            &target.join("lueying-settings.exe"),
            &["models", "fetch"],
            Some(target),
        ) {
            trace(&format!("install: spawn models fetch failed: {error}"));
        }
    }

    fn uninstall() {
        trace("uninstall: stop_server");
        stop_server();
        trace("uninstall: profile");
        let _ = set_profile_enabled(false);
        trace("uninstall: registry");
        remove_registry();
        trace("uninstall: cert");
        remove_code_sign_cert();
        trace("uninstall: prune");
        prune_on_uninstall();
        trace("uninstall: done");
    }

    /// 删除旧版 TSF 组件:版本化遗留与升级时改名让路的 .previous。
    /// 被宿主加载的文件删不掉,留给下次安装;失败不报错。
    fn prune() {
        let Ok(target) = app_dir() else {
            return;
        };
        prune_old_dlls(&target, "lueying_tsf.dll");
    }

    /// 卸载后再没有「下次安装」来收尾:删不掉的 TSF 组件直接排进重启删除队列。
    /// profile 激活后系统宿主会长期加载本 DLL,改名让路的 .previous 同样删不掉。
    fn prune_on_uninstall() {
        let Ok(target) = app_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&target) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let lower = name.to_ascii_lowercase();
            // 主 DLL 留给 Inno 的 regserver 注销与 uninsrestartdelete:
            // [UninstallRun] 先于 regsvr32 /u 执行,这里删掉会让注销找不到文件。
            if name.eq_ignore_ascii_case("lueying_tsf.dll") {
                continue;
            }
            if !lower.starts_with("lueying_tsf")
                || !(lower.ends_with(".dll") || lower.ends_with(".dll.previous"))
            {
                continue;
            }
            let path = entry.path();
            if std::fs::remove_file(&path).is_ok() {
                continue;
            }
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let scheduled =
                unsafe { MoveFileExW(PCWSTR(wide.as_ptr()), None, MOVEFILE_DELAY_UNTIL_REBOOT) }
                    .is_ok();
            trace(&format!("uninstall: 重启删除 {name} = {scheduled}"));
        }
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
                || !(lower.ends_with(".dll") || lower.ends_with(".dll.previous"))
            {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
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
                unsafe {
                    profiles.EnableLanguageProfile(
                        &CLSID_IME,
                        LANGID_ZH_CN,
                        &GUID_PROFILE,
                        true,
                    )?;
                }
                // EnableLanguageProfile 只写 HKLM 下的 Enable 标志，不会把 profile
                // 追加到当前用户的输入法列表；InstallLayoutOrTip 才写 HKCU\User Profile。
                // 只加入列表，不碰默认/会话激活：任务栏搜索等 shell 宿主跟的是
                // 会话默认输入法，安装器抢默认会让这些宿主再也切不走。
                let listed = call_input_dll(b"InstallLayoutOrTip\0", PROFILE_ID);
                trace(&format!("profile: listed={listed} default untouched"));
                return Ok(());
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

    /// 调 `input.dll` 里的布局/文本服务安装函数。Windows 没有为它们提供
    /// 稳定的链接库导入项，只能按名字取地址；缺失时返回 false 由安装流程继续。
    fn call_input_dll(procedure: &[u8], profile: &str) -> bool {
        type Proc = unsafe extern "system" fn(*const u16, u32) -> i32;
        unsafe {
            let Ok(module) = LoadLibraryW(w!("input.dll")) else {
                return false;
            };
            let Some(address) = GetProcAddress(module, PCSTR(procedure.as_ptr())) else {
                let _ = FreeLibrary(module);
                return false;
            };
            let function: Proc = std::mem::transmute(address);
            let wide: Vec<u16> = profile.encode_utf16().chain(std::iter::once(0)).collect();
            let ok = function(wide.as_ptr(), 0) != 0;
            let _ = FreeLibrary(module);
            ok
        }
    }

    /// 机器级签名资格:建/复用自签名代码签名证书(进机器受信任根与
    /// 受信任发布者),给 `lueying-server.exe` 盖上签名。带 uiAccess 清单的
    /// 本体没有受信任根下的签名时,进程创建会被系统直接拒绝——签名失败
    /// 等于装出一个打不了字的服务,所以这一步失败按安装失败处理。
    fn sign_server() -> Result<(), String> {
        let target = app_dir()?.join("lueying-server.exe");
        let script = SIGN_SCRIPT
            .replace("{cn}", CERT_SUBJECT)
            .replace("{target}", &ps_single_quoted(&target.display().to_string()));
        let output = run_script(&script)?;
        if !output.contains("Valid") {
            return Err(format!("签名验证未通过: {output}"));
        }
        Ok(())
    }

    /// 卸载收回机器信任:三处证书存储里按主题清掉自签名证书。
    /// 失败不拦卸载,痕迹进 trace。
    fn remove_code_sign_cert() {
        let script = REMOVE_CERT_SCRIPT.replace("{cn}", CERT_SUBJECT);
        match run_script(&script) {
            Ok(_) => trace("uninstall: 证书已移除"),
            Err(error) => trace(&format!("uninstall: 证书移除失败: {error}")),
        }
    }

    /// 用系统 PowerShell 5.1 跑一段脚本,返回 stdout。脚本临时落盘再 -File:
    /// -Command 的引号嵌套不可靠;UTF-8 BOM 让 5.1 按 UTF-8 读,中文路径不乱。
    fn run_script(script: &str) -> Result<String, String> {
        let path = std::env::temp_dir().join(format!("lueying-script-{}.ps1", std::process::id()));
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice(script.as_bytes());
        std::fs::write(&path, bytes).map_err(|e| format!("写脚本临时文件: {e}"))?;
        let output = Command::new(system32(r"WindowsPowerShell\v1.0\powershell.exe"))
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&path)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("运行 PowerShell: {e}"));
        let _ = std::fs::remove_file(&path);
        let output = output?;
        if !output.status.success() {
            return Err(format!(
                "PowerShell 退出码 {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// PowerShell 单引号字面量:' 是唯一要转义的字符。
    fn ps_single_quoted(text: &str) -> String {
        text.replace('\'', "''")
    }

    const SIGN_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$cn = '{cn}'
$target = '{target}'
$cert = Get-ChildItem Cert:\LocalMachine\My |
  Where-Object { $_.Subject -eq $cn -and $_.HasPrivateKey } |
  Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $cert) {
  $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $cn `
    -KeyUsage DigitalSignature -KeySpec Signature -KeyLength 3072 `
    -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(10)
  $cer = Join-Path $env:TEMP 'lueying-dev.cer'
  Export-Certificate -Cert $cert -FilePath $cer | Out-Null
  Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
  Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
  Remove-Item $cer -Force
}
Set-AuthenticodeSignature -FilePath $target -Certificate $cert | Out-Null
Write-Output (Get-AuthenticodeSignature -FilePath $target).Status
"#;

    const REMOVE_CERT_SCRIPT: &str = r#"$cn = '{cn}'
Get-ChildItem Cert:\LocalMachine\My, Cert:\LocalMachine\Root, Cert:\LocalMachine\TrustedPublisher |
  Where-Object { $_.Subject -eq $cn } | Remove-Item
"#;

    /// 先按当前用户的管道名请服务退出,再按进程名兜底。
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
        // 扩展宿主是服务端的子进程,升级替换文件前一并停掉;没有在跑时 taskkill 报错即可忽略。
        for image in ["lueying-server.exe", "lueying-ext-host.exe"] {
            trace("stop_server: taskkill");
            let mut command = Command::new(system32("taskkill.exe"));
            command
                .args(["/F", "/IM", image])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW);
            let _ = command.status();
        }
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
