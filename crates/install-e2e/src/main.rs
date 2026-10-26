//! Windows 安装/卸载冒烟测试:用发布目录编译真实 Inno Setup 安装包,指向隔离
//! 目录静默安装,检查 x64/x86 文件、自启动/卸载项与两套 TSF 注册;再用真实
//! 更新器升级到 9.9.9,最后运行卸载器确认清理。产物 `target/install-e2e-report.md`。

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
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, Instant};

    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
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
    /// Inno 的卸载项;AppId 与 installer/lueying.iss 必须一致。
    const INNO_UNINSTALL_KEY: &str =
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall\{5F0C8C3E-9B7A-4E2D-9A6B-2C1D8E4F7A10}_is1";

    pub(super) fn run() {
        let _ = std::fs::create_dir_all("target");
        let report = real_run().map_or_else(
            |e| format!("# 安装/卸载 E2E\n\nFAIL\n\n{e}\n"),
            |body| format!("# 安装/卸载 E2E\n\nPASS\n\n{body}\n"),
        );
        println!("{report}");
        let _ = std::fs::write("target/install-e2e-report.md", &report);
        if !report.contains("PASS") {
            std::process::exit(1);
        }
    }

    /// 阶段标记:CI 卡住时日志里最后一行就是卡住的位置。
    fn phase(text: &str) {
        println!("[install-e2e] {text}");
        let _ = std::io::stdout().flush();
    }

    /// 安装包、辅助程序与更新器共用的阶段跟踪文件;随 artifact 上传。
    fn trace_path() -> PathBuf {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("target/install-e2e-trace.log")
    }

    /// 跑子进程并限时。超时就杀掉并返回错误,避免整步被 CI 的超时静默终结。
    ///
    /// 输出写文件而不是管道:Windows 会把调用方所有可继承句柄复制给子进程,
    /// 被拉起的常驻服务若持有管道写端,父进程就永远读不到 EOF。
    fn run_command(name: &str, command: &mut Command, limit: Duration) -> Result<Output, String> {
        phase(&format!("{name}: 启动"));
        let logs = Path::new("target/install-e2e-logs");
        let _ = std::fs::create_dir_all(logs);
        let slug: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let out_path = logs.join(format!("{slug}.out"));
        let err_path = logs.join(format!("{slug}.err"));
        let out =
            std::fs::File::create(&out_path).map_err(|e| format!("创建{name}输出文件: {e}"))?;
        let err =
            std::fs::File::create(&err_path).map_err(|e| format!("创建{name}错误文件: {e}"))?;
        command.stdin(Stdio::null()).stdout(out).stderr(err);
        let mut child = command.spawn().map_err(|e| format!("启动{name}: {e}"))?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < limit => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{name} 超过 {} 秒未退出", limit.as_secs()));
                }
                Err(error) => return Err(format!("等待{name}: {error}")),
            }
        };
        phase(&format!("{name}: 退出 {:?}", status.code()));
        let stdout = std::fs::read(&out_path).unwrap_or_default();
        let stderr = std::fs::read(&err_path).unwrap_or_default();
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// 安装包、更新器与卸载器共用的隔离环境。
    fn apply_env(command: &mut Command, local: &Path, appdata: &Path, target: &Path, trace: &Path) {
        command
            .env("LOCALAPPDATA", local)
            .env("APPDATA", appdata)
            .env("LUEYING_INSTALL_DIR", target)
            // 与真实安装/其他 E2E 的常驻服务隔离。
            .env(
                "LUEYING_PIPE_SUFFIX",
                format!("install{}", std::process::id()),
            )
            // 测试只验证安装/注册/卸载;候选窗在无桌面 CI 上不启动。
            .env("LUEYING_SERVER_NO_UI", "1")
            .env("LUEYING_INSTALL_TRACE", trace);
    }

    fn real_run() -> Result<String, String> {
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            return Ok("SKIP: 当前进程不是管理员,无法注册 TSF;CI 应使用管理员 runner。".into());
        }
        let trace = trace_path();
        let _ = std::fs::remove_file(&trace);
        phase(&format!("trace={}", trace.display()));
        // 输入法列表查询走 TSF profile 管理器;已是别的套间模式时也能继续查。
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let bin = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("无父目录")?
            .to_path_buf();
        let root = std::env::temp_dir().join(format!("lueying-install-e2e-{}", std::process::id()));
        let payload = root.join("payload");
        let dist = root.join("dist");
        let local = root.join("local");
        let appdata = root.join("appdata");
        let program = root.join("program-files");
        std::fs::create_dir_all(&payload).map_err(|e| format!("创建 payload: {e}"))?;
        std::fs::create_dir_all(&local).map_err(|e| format!("创建 LOCALAPPDATA: {e}"))?;
        std::fs::create_dir_all(&appdata).map_err(|e| format!("创建 APPDATA: {e}"))?;
        std::fs::create_dir_all(&program).map_err(|e| format!("创建 Program Files: {e}"))?;

        let version = env!("CARGO_PKG_VERSION");
        stage_payload(&bin, &payload, version)?;
        let target = program.join("lueying");
        let result = (|| -> Result<String, String> {
            phase("编译首次安装包");
            let setup = compile_setup(&payload, version, &dist)?;

            phase("首次安装");
            run_setup(
                "首次安装",
                &setup,
                &target,
                &root.join("install.log"),
                &local,
                &appdata,
                &trace,
            )?;
            phase("首次安装: 完成");
            phase("检查安装结果");

            let mut checks = Vec::new();
            for name in [
                "lueying-user.exe",
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
            let dll64 = format!("lueying_tsf-{}.dll", safe_version(version));
            let dll32 = format!("lueying_tsf32-{}.dll", safe_version(version));
            for name in [&dll64, &dll32] {
                if !target.join(name).is_file() {
                    return Err(format!("安装目录缺少 {name}"));
                }
                checks.push(format!("TSF DLL {name}"));
            }

            if !reg_query(RUN_KEY, Some("Lueying")) {
                return Err("缺少 HKCU 自启动项".into());
            }
            checks.push("自启动项".into());
            if !reg_query(INNO_UNINSTALL_KEY, None) {
                return Err("缺少 Inno 卸载项".into());
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
            // 注册 profile 不等于进入用户输入法列表;辅助程序必须显式启用,
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

            phase("更新器升级");
            update_installed(&payload, version, &target, &root, &dist, &local, &appdata, &trace)?;
            checks.push("更新器升级到 9.9.9".into());

            phase("卸载");
            uninstall(&target, &root, &local, &appdata, &trace)?;
            phase("等待卸载清理");
            let deadline = Instant::now() + Duration::from_secs(90);
            loop {
                let cleaned = !target.exists()
                    && !reg_query(INNO_UNINSTALL_KEY, None)
                    && !reg_query(RUN_KEY, Some("Lueying"))
                    && !reg_query(&inproc64, Some(""))
                    && !reg_query(&inproc32, Some(""));
                if cleaned || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if target.exists() {
                return Err(format!("卸载后目录仍在: {}", target.display()));
            }
            if reg_query(INNO_UNINSTALL_KEY, None) {
                return Err("卸载后卸载项仍在".into());
            }
            if reg_query(RUN_KEY, Some("Lueying")) {
                return Err("卸载后自启动项仍在".into());
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
            phase("清理: 尝试卸载");
            let _ = cleanup_install(&target, &payload, &local, &appdata, &trace);
        }
        let _ = std::fs::remove_dir_all(&root);
        result.map(|checks| format!("- {checks}\n"))
    }

    /// 复制构建产物与许可文件。发布目录的 DLL 用原名,由 Inno 在安装时改名。
    fn stage_payload(bin: &Path, payload: &Path, version: &str) -> Result<(), String> {
        for name in [
            "lueying-user.exe",
            "lueying-server.exe",
            "lueying-settings.exe",
            "lueying-update.exe",
            "lueying_tsf.dll",
            "lueying_tsf32.dll",
        ] {
            std::fs::copy(bin.join(name), payload.join(name))
                .map_err(|e| format!("复制 {name}: {e}"))?;
        }
        std::fs::write(payload.join("version.txt"), version)
            .map_err(|e| format!("写 version.txt: {e}"))?;
        let license = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../LICENSE");
        std::fs::copy(&license, payload.join("LICENSE"))
            .map_err(|e| format!("复制 LICENSE: {e}"))?;
        let licenses = payload.join("licenses");
        std::fs::create_dir_all(&licenses).map_err(|e| format!("创建 licenses: {e}"))?;
        std::fs::write(licenses.join("README.md"), "第三方许可见此处。\n")
            .map_err(|e| format!("写 licenses/README.md: {e}"))?;
        Ok(())
    }

    fn compile_setup(payload: &Path, version: &str, out: &Path) -> Result<PathBuf, String> {
        let iscc = iscc_path()?;
        std::fs::create_dir_all(out).map_err(|e| format!("创建 {}: {e}", out.display()))?;
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../installer/lueying.iss");
        let setup = out.join(format!("lueying-{version}-setup.exe"));
        let _ = std::fs::remove_file(&setup);
        let mut command = Command::new(iscc);
        command
            .arg(format!("/DPayloadDir={}", payload.display()))
            .arg(format!("/DOutDir={}", out.display()))
            .arg(format!("/DVersion={version}"))
            .arg(format!("/DVersionSafe={}", safe_version(version)))
            .arg(format!("/DVersionInfo={}", version_info(version)))
            .arg(&script);
        let output = run_command("ISCC", &mut command, Duration::from_secs(180))?;
        if !output.status.success() {
            return Err(format!(
                "ISCC 退出码 {:?}: {}{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if !setup.is_file() {
            return Err(format!("ISCC 没有产出 {}", setup.display()));
        }
        Ok(setup)
    }

    /// CI 通过 `LUEYING_ISCC` 指定;本地默认 Inno Setup 6 的安装位置。
    fn iscc_path() -> Result<PathBuf, String> {
        if let Some(path) = std::env::var_os("LUEYING_ISCC").map(PathBuf::from) {
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!("LUEYING_ISCC 指向的 {} 不存在", path.display()));
        }
        [
            PathBuf::from(r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe"),
            PathBuf::from(r"C:\Program Files\Inno Setup 6\ISCC.exe"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "找不到 ISCC.exe;安装 Inno Setup 6 或设置 LUEYING_ISCC".to_string())
    }

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

    fn version_info(version: &str) -> String {
        let core = version.split('-').next().unwrap_or(version);
        let mut parts = core
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>();
        parts.resize(4, 0);
        format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3])
    }

    fn run_setup(
        name: &str,
        setup: &Path,
        target: &Path,
        log: &Path,
        local: &Path,
        appdata: &Path,
        trace: &Path,
    ) -> Result<(), String> {
        let mut command = Command::new(setup);
        command
            .arg("/VERYSILENT")
            .arg("/SUPPRESSMSGBOXES")
            .arg("/NORESTART")
            .arg("/LANG=chinesesimplified")
            .arg(format!("/DIR=\"{}\"", target.display()))
            .arg(format!("/LOG=\"{}\"", log.display()));
        apply_env(&mut command, local, appdata, target, trace);
        let output = run_command(name, &mut command, Duration::from_secs(150))?;
        if output.status.success() {
            Ok(())
        } else {
            let detail = std::fs::read(log)
                .map(|bytes| tail(&bytes))
                .unwrap_or_default();
            Err(format!(
                "{name} 退出码 {:?}: {detail}",
                output.status.code()
            ))
        }
    }

    /// 用本地 9.9.9 安装包跑一遍真实更新器:校验、静默运行 Inno 安装包替换版本。
    #[allow(clippy::too_many_arguments)]
    fn update_installed(
        payload: &Path,
        version: &str,
        target: &Path,
        root: &Path,
        dist: &Path,
        local: &Path,
        appdata: &Path,
        trace: &Path,
    ) -> Result<(), String> {
        let package = root.join("update-payload");
        std::fs::create_dir_all(&package).map_err(|e| format!("创建更新包: {e}"))?;
        for name in [
            "lueying-user.exe",
            "lueying-server.exe",
            "lueying-settings.exe",
            "lueying-update.exe",
            "lueying_tsf.dll",
            "lueying_tsf32.dll",
            "LICENSE",
        ] {
            std::fs::copy(payload.join(name), package.join(name))
                .map_err(|e| format!("复制更新包 {name}: {e}"))?;
        }
        let licenses = package.join("licenses");
        std::fs::create_dir_all(&licenses).map_err(|e| format!("创建更新包 licenses: {e}"))?;
        std::fs::copy(
            payload.join("licenses/README.md"),
            licenses.join("README.md"),
        )
        .map_err(|e| format!("复制更新包许可证: {e}"))?;
        std::fs::write(package.join("version.txt"), "9.9.9\n")
            .map_err(|e| format!("写更新包 version.txt: {e}"))?;

        phase("编译 9.9.9 安装包");
        let setup = compile_setup(&package, "9.9.9", dist)?;
        let sha = sha256_file(&setup)?;

        // 从安装目录运行会锁住 lueying-update.exe,导致新安装包复制失败;
        // 正式流程也是先复制到 %TEMP% 再运行。
        let updater_dir = root.join("updater");
        std::fs::create_dir_all(&updater_dir).map_err(|e| format!("创建更新器目录: {e}"))?;
        let updater = updater_dir.join("lueying-update.exe");
        std::fs::copy(target.join("lueying-update.exe"), &updater)
            .map_err(|e| format!("复制更新器: {e}"))?;

        let mut command = Command::new(&updater);
        command
            .arg("--installer")
            .arg(&setup)
            .args(["--sha256", sha.as_str(), "--version", "9.9.9", "--quiet"]);
        apply_env(&mut command, local, appdata, target, trace);
        let output = run_command("更新器", &mut command, Duration::from_secs(180))?;
        if !output.status.success() {
            return Err(format!(
                "更新器退出码 {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let installed = std::fs::read_to_string(target.join("version.txt")).unwrap_or_default();
        if installed.trim() != "9.9.9" {
            return Err(format!("更新后版本是 {},期望 9.9.9", installed.trim()));
        }
        for name in ["lueying_tsf-9.9.9.dll", "lueying_tsf32-9.9.9.dll"] {
            if !target.join(name).is_file() {
                return Err(format!("更新后缺少 {name}"));
            }
        }
        let old = format!("lueying_tsf-{}.dll", safe_version(version));
        if target.join(old).exists() {
            return Err(format!("更新后仍留着旧 DLL {old}"));
        }
        Ok(())
    }

    fn uninstall(
        target: &Path,
        root: &Path,
        local: &Path,
        appdata: &Path,
        trace: &Path,
    ) -> Result<(), String> {
        let uninstaller = target.join("unins000.exe");
        if !uninstaller.is_file() {
            return Err("安装目录缺少 unins000.exe".into());
        }
        let log = root.join("uninstall.log");
        let mut command = Command::new(&uninstaller);
        command
            .arg("/VERYSILENT")
            .arg("/SUPPRESSMSGBOXES")
            .arg("/NORESTART")
            .arg(format!("/LOG=\"{}\"", log.display()));
        apply_env(&mut command, local, appdata, target, trace);
        let output = run_command("卸载器", &mut command, Duration::from_secs(120))?;
        if output.status.success() {
            Ok(())
        } else {
            let detail = std::fs::read(&log)
                .map(|bytes| tail(&bytes))
                .unwrap_or_default();
            Err(format!(
                "卸载器退出码 {:?}: {detail}",
                output.status.code()
            ))
        }
    }

    fn cleanup_install(
        target: &Path,
        payload: &Path,
        local: &Path,
        appdata: &Path,
        trace: &Path,
    ) -> Result<(), String> {
        let uninstaller = target.join("unins000.exe");
        if uninstaller.is_file() {
            let mut command = Command::new(&uninstaller);
            command
                .arg("/VERYSILENT")
                .arg("/SUPPRESSMSGBOXES")
                .arg("/NORESTART");
            apply_env(&mut command, local, appdata, target, trace);
            let _ = run_command("清理卸载", &mut command, Duration::from_secs(60));
        }
        let helper = target.join("lueying-user.exe");
        let helper = if helper.is_file() {
            helper
        } else {
            payload.join("lueying-user.exe")
        };
        if helper.is_file() {
            let mut command = Command::new(helper);
            command.arg("--stop-server");
            apply_env(&mut command, local, appdata, target, trace);
            let _ = run_command("清理停止服务", &mut command, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(target);
        Ok(())
    }

    /// 与更新器相同的 certutil 路径,验证 SHA-256 校验分支。
    fn sha256_file(path: &Path) -> Result<String, String> {
        let mut command = Command::new(system32("certutil.exe"));
        command.arg("-hashfile").arg(path).arg("SHA256");
        let output = run_command("certutil", &mut command, Duration::from_secs(30))?;
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

    fn tail(bytes: &[u8]) -> String {
        let start = bytes.len().saturating_sub(800);
        String::from_utf8_lossy(&bytes[start..]).trim().to_string()
    }

    fn system32(name: &str) -> PathBuf {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join(name)
    }

    /// TSF profile 是否在当前用户的输入法列表里。profile 管理器在 CI 会话里
    /// 可能一直不返回,限时查询,别把 E2E 拖住。
    fn profile_enabled() -> Result<bool, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(profile_enabled_blocking());
        });
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|_| "查询输入法列表超过 3 秒".to_string())?
    }

    fn profile_enabled_blocking() -> Result<bool, String> {
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let result = (|| -> Result<bool, String> {
            let profiles: ITfInputProcessorProfiles = unsafe {
                CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
            }
            .map_err(|e| format!("打开 TSF profile 管理器: {e}"))?;
            let enabled = unsafe {
                profiles.IsEnabledLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE)
            }
            .map_err(|e| format!("查询当前用户输入法列表: {e}"))?;
            Ok(enabled.as_bool())
        })();
        if initialized {
            unsafe { CoUninitialize() };
        }
        result
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
        let output = run_command("reg query", &mut command, Duration::from_secs(15)).ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
