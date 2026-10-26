//! Windows 安装/卸载冒烟测试:把真实安装器指向隔离的 `LUEYING_INSTALL_DIR`,
//! 检查 x64/x86 文件、自启动/卸载项与两套 TSF 注册,再执行真实卸载并确认清理。
//! 产物 `target/install-e2e-report.md`。

fn main() {
    #[cfg(windows)]
    {
        imp::run();
    }
    #[cfg(not(windows))]
    println!("lueying-install-e2e 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::IsUserAnAdmin;
    use windows::Win32::UI::TextServices::{
        CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfiles,
    };
    use windows::core::GUID;

    const CLSID: &str = "{8FFECD5B-DBA4-480C-9749-FECDCAA8FA17}";
    const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcaa8fa17);
    const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);
    const LANGID_ZH_CN: u16 = 0x0804;
    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Lueying";

    pub(super) fn run() {
        let report = real_run().map_or_else(
            |e| format!("# 安装/卸载 E2E\n\nFAIL\n\n{e}\n"),
            |body| format!("# 安装/卸载 E2E\n\nPASS\n\n{body}\n"),
        );
        println!("{report}");
        let _ = std::fs::create_dir_all("target");
        let _ = std::fs::write("target/install-e2e-report.md", &report);
        if !report.contains("PASS") {
            std::process::exit(1);
        }
    }

    fn real_run() -> Result<String, String> {
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            return Ok("SKIP: 当前进程不是管理员,无法注册 TSF;CI 应使用管理员 runner。".into());
        }
        // 输入法列表查询走 TSF profile 管理器;已是别的套间模式时也能继续查。
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let bin = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("无父目录")?
            .to_path_buf();
        let root = std::env::temp_dir().join(format!("lueying-install-e2e-{}", std::process::id()));
        let payload = root.join("payload");
        let local = root.join("local");
        let appdata = root.join("appdata");
        let program = root.join("program-files");
        std::fs::create_dir_all(&payload).map_err(|e| format!("创建 payload: {e}"))?;
        std::fs::create_dir_all(&local).map_err(|e| format!("创建 LOCALAPPDATA: {e}"))?;
        std::fs::create_dir_all(&appdata).map_err(|e| format!("创建 APPDATA: {e}"))?;
        std::fs::create_dir_all(&program).map_err(|e| format!("创建 Program Files: {e}"))?;
        for name in [
            "lueying-install.exe",
            "lueying-server.exe",
            "lueying-settings.exe",
            "lueying-update.exe",
            "lueying_tsf.dll",
            "lueying_tsf32.dll",
        ] {
            std::fs::copy(bin.join(name), payload.join(name))
                .map_err(|e| format!("复制 {name}: {e}"))?;
        }
        std::fs::write(payload.join("version.txt"), env!("CARGO_PKG_VERSION"))
            .map_err(|e| format!("写 version.txt: {e}"))?;
        // 发布包里的许可子目录必须一起进安装目录;这里用一份样例覆盖递归复制。
        let licenses = payload.join("licenses");
        std::fs::create_dir_all(&licenses).map_err(|e| format!("创建 licenses: {e}"))?;
        std::fs::write(licenses.join("README.md"), "第三方许可见此处。\n")
            .map_err(|e| format!("写 licenses/README.md: {e}"))?;

        let target = program.join("lueying");
        let result = (|| -> Result<String, String> {
            let status = Command::new(payload.join("lueying-install.exe"))
                .arg("--quiet")
                .env("LOCALAPPDATA", &local)
                .env("APPDATA", &appdata)
                .env("LUEYING_INSTALL_DIR", &target)
                // 与真实安装/其他 E2E 的常驻服务隔离。
                .env(
                    "LUEYING_PIPE_SUFFIX",
                    format!("install{}", std::process::id()),
                )
                // 测试只验证安装/注册/卸载;候选窗在无桌面 CI 上不启动。
                .env("LUEYING_SERVER_NO_UI", "1")
                .status()
                .map_err(|e| format!("启动安装器: {e}"))?;
            if !status.success() {
                return Err(format!("安装器退出码 {:?}", status.code()));
            }

            let mut checks = Vec::new();
            for name in [
                "lueying-install.exe",
                "lueying-server.exe",
                "lueying-settings.exe",
                "lueying-update.exe",
                "version.txt",
                "licenses/README.md",
            ] {
                if !target.join(name).exists() {
                    return Err(format!("安装目录缺少 {name}"));
                }
                checks.push(format!("文件 {name}"));
            }
            let mut dll64 = None;
            let mut dll32 = None;
            for entry in std::fs::read_dir(&target)
                .map_err(|e| format!("读取安装目录: {e}"))?
                .flatten()
            {
                let name = entry.file_name().to_string_lossy().to_string();
                let lower = name.to_ascii_lowercase();
                if !lower.starts_with("lueying_tsf") || !lower.ends_with(".dll") {
                    continue;
                }
                if lower.starts_with("lueying_tsf32") {
                    dll32 = Some(name);
                } else {
                    dll64 = Some(name);
                }
            }
            let dll64 = dll64.ok_or("安装目录没有版本化 x64 TSF DLL")?;
            let dll32 = dll32.ok_or("安装目录没有版本化 x86 TSF DLL")?;
            checks.push(format!("x64 TSF DLL {dll64}"));
            checks.push(format!("x86 TSF DLL {dll32}"));

            if !reg_query(RUN_KEY, Some("Lueying")) {
                return Err("缺少 HKCU 自启动项".into());
            }
            checks.push("自启动项".into());
            if !reg_query(UNINSTALL_KEY, None) {
                return Err("缺少 HKCU 卸载项".into());
            }
            checks.push("卸载项".into());
            let inproc64 = format!(r"HKCR\CLSID\{CLSID}\InprocServer32");
            let inproc32 = format!(r"HKCR\WOW6432Node\CLSID\{CLSID}\InprocServer32");
            let target_text = target.to_string_lossy().to_ascii_lowercase();
            for (key, label) in [(&inproc64, "x64"), (&inproc32, "x86")] {
                let value = reg_read(key, Some(""))
                    .ok_or_else(|| format!("缺少 {label} TSF CLSID InprocServer32"))?;
                if !value.to_ascii_lowercase().contains(&target_text) {
                    return Err(format!("{label} TSF 注册指向了错误路径: {value}"));
                }
            }
            checks.push("x64/x86 TSF 注册指向安装目录".into());
            // 注册 profile 不等于进入用户输入法列表;安装器必须显式启用,
            // 否则装完按 Win+空格 找不到「掠影」。profile 管理器有缓存,短暂重试。
            let mut enabled = false;
            for _ in 0..10 {
                enabled = profile_enabled()?;
                if enabled {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if !enabled {
                return Err("安装后「掠影」未加入当前用户输入法列表".into());
            }
            checks.push("已加入当前用户输入法列表".into());

            update_installed(&payload, &target, &root, &local, &appdata)?;
            checks.push("更新器升级到 9.9.9".into());

            let installed = target.join("lueying-install.exe");
            let status = Command::new(&installed)
                .args(["--uninstall", "--quiet"])
                .env("LOCALAPPDATA", &local)
                .env("APPDATA", &appdata)
                .env("LUEYING_INSTALL_DIR", &target)
                .env(
                    "LUEYING_PIPE_SUFFIX",
                    format!("install{}", std::process::id()),
                )
                .status()
                .map_err(|e| format!("启动卸载器: {e}"))?;
            if !status.success() {
                return Err(format!("卸载器退出码 {:?}", status.code()));
            }
            // 安装器自身占着目录时会把删除交给后台 cmd;等它收尾。
            let deadline = Instant::now() + Duration::from_secs(20);
            while target.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
            if target.exists() {
                return Err(format!("卸载后目录仍在: {}", target.display()));
            }
            checks.push("文件已删除".into());
            if reg_query(RUN_KEY, Some("Lueying")) {
                return Err("卸载后自启动项仍在".into());
            }
            if reg_query(UNINSTALL_KEY, None) {
                return Err("卸载后卸载项仍在".into());
            }
            if reg_query(&inproc64, Some("")) || reg_query(&inproc32, Some("")) {
                return Err("卸载后 TSF CLSID 仍在".into());
            }
            // profile 已注销时查询本身可能失败;只有明确仍启用才算清理失败。
            if profile_enabled().unwrap_or(false) {
                return Err("卸载后「掠影」仍在当前用户输入法列表".into());
            }
            checks.push("注册项与输入法列表已清理".into());
            Ok(checks.join("\n- "))
        })();

        // 失败时尽力卸载,别把 runner 的注册表/目录留脏。
        if result.is_err() {
            let installed = target.join("lueying-install.exe");
            let installer = if installed.exists() {
                installed
            } else {
                payload.join("lueying-install.exe")
            };
            let _ = Command::new(installer)
                .args(["--uninstall", "--quiet"])
                .env("LOCALAPPDATA", &local)
                .env("APPDATA", &appdata)
                .env("LUEYING_INSTALL_DIR", &target)
                .env(
                    "LUEYING_PIPE_SUFFIX",
                    format!("install{}", std::process::id()),
                )
                .status();
        }
        let _ = std::fs::remove_dir_all(&root);
        result.map(|checks| format!("- {checks}\n"))
    }

    /// 用本地 9.9.9 包跑一遍真实更新器:解压、校验、调用新安装器替换版本。
    fn update_installed(
        payload: &Path,
        target: &Path,
        root: &Path,
        local: &Path,
        appdata: &Path,
    ) -> Result<(), String> {
        let package = root.join("update-package");
        std::fs::create_dir_all(&package).map_err(|e| format!("创建更新包: {e}"))?;
        for name in [
            "lueying-install.exe",
            "lueying-server.exe",
            "lueying-settings.exe",
            "lueying-update.exe",
        ] {
            std::fs::copy(payload.join(name), package.join(name))
                .map_err(|e| format!("复制更新包 {name}: {e}"))?;
        }
        std::fs::copy(
            payload.join("lueying_tsf.dll"),
            package.join("lueying_tsf-9.9.9.dll"),
        )
        .map_err(|e| format!("复制更新包 x64 DLL: {e}"))?;
        std::fs::copy(
            payload.join("lueying_tsf32.dll"),
            package.join("lueying_tsf32-9.9.9.dll"),
        )
        .map_err(|e| format!("复制更新包 x86 DLL: {e}"))?;
        std::fs::write(package.join("version.txt"), "9.9.9\n")
            .map_err(|e| format!("写更新包 version.txt: {e}"))?;
        std::fs::write(
            package.join("manifest.json"),
            r#"{"name":"lueying","version":"9.9.9","files":[]}"#,
        )
        .map_err(|e| format!("写更新包 manifest: {e}"))?;

        let zip = root.join("lueying-9.9.9.zip");
        let status = Command::new(system32("tar.exe"))
            .args(["-a", "-cf"])
            .arg(&zip)
            .arg("-C")
            .arg(&package)
            .arg(".")
            .status()
            .map_err(|e| format!("创建更新 zip: {e}"))?;
        if !status.success() {
            return Err(format!("tar 打包更新 zip 退出码 {:?}", status.code()));
        }
        let sha = sha256_file(&zip)?;

        // 从安装目录运行会锁住 lueying-update.exe,导致新安装器复制失败;
        // 正式流程也是先复制到 %TEMP% 再运行。
        let updater_dir = root.join("updater");
        std::fs::create_dir_all(&updater_dir).map_err(|e| format!("创建更新器目录: {e}"))?;
        let updater = updater_dir.join("lueying-update.exe");
        std::fs::copy(target.join("lueying-update.exe"), &updater)
            .map_err(|e| format!("复制更新器: {e}"))?;

        let output = Command::new(&updater)
            .arg("--zip")
            .arg(&zip)
            .args(["--sha256", sha.as_str(), "--version", "9.9.9", "--quiet"])
            .env("LOCALAPPDATA", local)
            .env("APPDATA", appdata)
            .env("LUEYING_INSTALL_DIR", target)
            .env(
                "LUEYING_PIPE_SUFFIX",
                format!("install{}", std::process::id()),
            )
            .env("LUEYING_SERVER_NO_UI", "1")
            .output()
            .map_err(|e| format!("启动更新器: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "更新器退出码 {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let version = std::fs::read_to_string(target.join("version.txt")).unwrap_or_default();
        if version.trim() != "9.9.9" {
            return Err(format!("更新后版本是 {},期望 9.9.9", version.trim()));
        }
        for name in ["lueying_tsf-9.9.9.dll", "lueying_tsf32-9.9.9.dll"] {
            if !target.join(name).is_file() {
                return Err(format!("更新后缺少 {name}"));
            }
        }
        Ok(())
    }

    /// 与更新器相同的 certutil 路径,验证 SHA-256 校验分支。
    fn sha256_file(path: &Path) -> Result<String, String> {
        let output = Command::new(system32("certutil.exe"))
            .arg("-hashfile")
            .arg(path)
            .arg("SHA256")
            .output()
            .map_err(|e| format!("运行 certutil: {e}"))?;
        if !output.status.success() {
            return Err(format!("certutil 退出码 {:?}", output.status.code()));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .map(|line| {
                line.chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
            })
            .find(|line| line.len() == 64 && line.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map(|line| line.to_ascii_lowercase())
            .ok_or_else(|| "certutil 输出里没有 SHA-256".into())
    }

    fn system32(name: &str) -> std::path::PathBuf {
        std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join(name)
    }

    /// TSF profile 是否在当前用户的输入法列表里。
    fn profile_enabled() -> Result<bool, String> {
        let profiles: ITfInputProcessorProfiles = unsafe {
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
        }
        .map_err(|e| format!("打开 TSF profile 管理器: {e}"))?;
        let enabled =
            unsafe { profiles.IsEnabledLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE) }
                .map_err(|e| format!("查询当前用户输入法列表: {e}"))?;
        Ok(enabled.as_bool())
    }

    /// `reg query` 是否成功;空值名用 `/ve` 查默认值。
    fn reg_query(key: &str, value: Option<&str>) -> bool {
        reg_read(key, value).is_some()
    }

    fn reg_read(key: &str, value: Option<&str>) -> Option<String> {
        let mut command = Command::new("reg");
        command.args(["query", key]);
        match value {
            Some("") => {
                command.arg("/ve");
            }
            Some(name) => {
                command.args(["/v", name]);
            }
            None => {}
        }
        let output = command.output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
