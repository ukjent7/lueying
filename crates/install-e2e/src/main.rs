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
    use std::process::Command;
    use std::time::{Duration, Instant};

    use windows::Win32::UI::Shell::IsUserAnAdmin;

    const CLSID: &str = "{8FFECD5B-DBA4-480C-9749-FECDCAA8FA17}";
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
            "lueying_tsf.dll",
            "lueying_tsf32.dll",
        ] {
            std::fs::copy(bin.join(name), payload.join(name))
                .map_err(|e| format!("复制 {name}: {e}"))?;
        }
        std::fs::write(payload.join("version.txt"), env!("CARGO_PKG_VERSION"))
            .map_err(|e| format!("写 version.txt: {e}"))?;

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
                "version.txt",
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
            checks.push("注册项已清理".into());
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
