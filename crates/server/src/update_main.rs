//! 独立更新器:等待设置窗退出,下载并校验 GitHub Release 里的 Inno 安装包,
//! 再静默运行它完成替换。更新器自身从临时副本运行,安装目录里的 exe 才不会被
//! 自己锁住。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::run();
    #[cfg(not(windows))]
    println!("lueying-update 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use lueying_server::update;
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW,
    };
    use windows::core::HSTRING;

    const APP: &str = "掠影更新";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[derive(Default)]
    struct Options {
        installer: Option<PathBuf>,
        install_dir: Option<PathBuf>,
        url: Option<String>,
        sha_url: Option<String>,
        sha256: Option<String>,
        version: Option<String>,
        size: u64,
        wait_pid: Option<u32>,
        quiet: bool,
        check: bool,
    }

    pub(super) fn run() {
        let options = match Options::parse() {
            Ok(options) => options,
            Err(error) => {
                message(APP, &format!("更新参数错误: {error}"), true, false);
                cleanup_self();
                std::process::exit(2);
            }
        };
        if options.check {
            check(&options);
            cleanup_self();
            return;
        }
        let version = match options.version.clone() {
            Some(version) if !version.is_empty() => version,
            _ => fail("缺少 --version", options.quiet),
        };
        let temp = std::env::temp_dir().join(format!("lueying-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        if let Err(error) = std::fs::create_dir_all(&temp) {
            fail(&format!("创建更新临时目录: {error}"), options.quiet);
        }
        let log_path = temp.join("update.log");
        let mut log = match File::create(&log_path) {
            Ok(file) => file,
            Err(error) => fail(&format!("创建更新日志: {error}"), options.quiet),
        };
        log_line(&mut log, &format!("更新到 {version}"));
        match install(&options, &version, &temp, &mut log) {
            Ok(()) => {
                log_line(&mut log, "完成");
                message(
                    APP,
                    &format!("掠影已更新到 {version}。"),
                    false,
                    options.quiet,
                );
                // 日志文件开着时 Windows 删不掉所在目录。
                drop(log);
                let _ = std::fs::remove_dir_all(&temp);
            }
            Err(error) => {
                log_line(&mut log, &format!("失败: {error}"));
                message(
                    APP,
                    &format!("更新失败: {error}\n\n日志:\n{}", log_path.display()),
                    true,
                    options.quiet,
                );
                cleanup_self();
                std::process::exit(1);
            }
        }
        cleanup_self();
    }

    fn check(options: &Options) {
        match update::check_channel(lueying_server::config::Channel::Stable) {
            Ok(update::Check::Latest) => message(
                APP,
                &format!("已是最新版本 {}。", update::current_version()),
                false,
                options.quiet,
            ),
            Ok(update::Check::NoRelease) => {
                message(APP, "GitHub 上还没有发布版本。", false, options.quiet)
            }
            Ok(update::Check::Available(release)) => message(
                APP,
                &format!(
                    "发现新版本 {}。\n\n打开设置窗口点「安装」即可升级。",
                    release.version
                ),
                false,
                options.quiet,
            ),
            Err(error) => {
                message(APP, &format!("检查更新失败: {error}"), true, options.quiet);
                cleanup_self();
                std::process::exit(1);
            }
        }
    }

    fn install(
        options: &Options,
        version: &str,
        temp: &Path,
        log: &mut File,
    ) -> Result<(), String> {
        if let Some(pid) = options.wait_pid {
            log_line(log, &format!("等待设置窗口 {pid} 退出"));
            update::wait_for_process(pid, 30_000)?;
        }
        let installer = match &options.installer {
            Some(path) => {
                if let Some(expected) = &options.sha256 {
                    verify_sha(path, expected)?;
                }
                path.clone()
            }
            None => {
                let url = options.url.as_deref().ok_or("缺少 --url 或 --installer")?;
                let dest = temp.join(format!("lueying-{}-setup.exe", safe_name(version)));
                log_line(log, &format!("下载 {url}"));
                update::download(url, &dest, options.size)?;
                log_line(log, "校验 SHA-256");
                let expected = match &options.sha256 {
                    Some(sha) => sha.clone(),
                    None => {
                        let sha_url = options
                            .sha_url
                            .as_deref()
                            .ok_or("缺少 --sha-url 或 --sha256")?;
                        update::fetch_sha256(sha_url)?
                    }
                };
                verify_sha(&dest, &expected)?;
                dest
            }
        };
        let target = options
            .install_dir
            .clone()
            .unwrap_or_else(update::install_target);
        let setup_log = temp.join("setup.log");
        log_line(log, &format!("运行安装包 {}", installer.display()));
        let mut command = Command::new(&installer);
        command.current_dir(temp).args([
            "/VERYSILENT".to_string(),
            "/SUPPRESSMSGBOXES".to_string(),
            "/NORESTART".to_string(),
            format!("/DIR={}", target.display()),
            format!("/LOG={}", setup_log.display()),
        ]);
        let output = command.output().map_err(|e| format!("启动安装包: {e}"))?;
        if !output.status.success() {
            let detail = std::fs::read(&setup_log)
                .map(|bytes| tail(&bytes))
                .unwrap_or_default();
            return Err(if detail.is_empty() {
                format!("安装包退出码 {:?}", output.status.code())
            } else {
                format!("安装包退出码 {:?}: {detail}", output.status.code())
            });
        }
        let installed = std::fs::read_to_string(target.join("version.txt")).unwrap_or_default();
        if installed.trim() != version {
            return Err(format!("安装后版本为 {},期望 {version}", installed.trim()));
        }
        if !target.join("lueying-update.exe").is_file() {
            return Err("安装目录缺少更新后的 lueying-update.exe".into());
        }
        Ok(())
    }

    fn verify_sha(path: &Path, expected: &str) -> Result<(), String> {
        let actual = update::sha256_file(path)?;
        if actual.eq_ignore_ascii_case(expected) {
            Ok(())
        } else {
            Err(format!("SHA-256 不匹配: 期望 {expected},实际 {actual}"))
        }
    }

    /// 从 `%TEMP%\lueying-update-<pid>` 启动时,退出后删掉这份临时副本。
    fn cleanup_self() {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let Some(dir) = exe.parent() else {
            return;
        };
        let name = dir.file_name().and_then(|name| name.to_str()).unwrap_or("");
        if !name.starts_with("lueying-update-") {
            return;
        }
        // 退出后由 PowerShell 删除;单引号路径能安全处理 &、括号等字符。
        let script = format!(
            "Start-Sleep -Seconds 1; Remove-Item -LiteralPath {} -Recurse -Force \
             -ErrorAction SilentlyContinue",
            ps_quote(dir)
        );
        let _ = Command::new(system32(r"WindowsPowerShell\v1.0\powershell.exe"))
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script.as_str(),
            ])
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }

    fn ps_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "''"))
    }

    fn system32(name: &str) -> PathBuf {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join(name)
    }

    /// 安装包失败时只把日志尾部写进错误信息,避免把整段输出塞进弹窗。
    fn tail(bytes: &[u8]) -> String {
        let start = bytes.len().saturating_sub(400);
        String::from_utf8_lossy(&bytes[start..]).trim().to_string()
    }

    fn safe_name(version: &str) -> String {
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

    fn log_line(log: &mut File, text: &str) {
        let _ = writeln!(log, "{text}");
        let _ = log.flush();
    }

    fn fail(text: &str, quiet: bool) -> ! {
        message(APP, text, true, quiet);
        cleanup_self();
        std::process::exit(1);
    }

    fn message(title: &str, text: &str, error: bool, quiet: bool) {
        if quiet {
            eprintln!("{title}: {text}");
            return;
        }
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

    impl Options {
        fn parse() -> Result<Self, String> {
            let mut options = Self::default();
            let mut args = std::env::args().skip(1);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--check" => options.check = true,
                    "--quiet" => options.quiet = true,
                    "--installer" => {
                        options.installer = Some(PathBuf::from(value(&mut args, "--installer")?));
                    }
                    "--install-dir" => {
                        options.install_dir =
                            Some(PathBuf::from(value(&mut args, "--install-dir")?));
                    }
                    "--url" => options.url = Some(value(&mut args, "--url")?),
                    "--sha-url" => options.sha_url = Some(value(&mut args, "--sha-url")?),
                    "--sha256" => options.sha256 = Some(value(&mut args, "--sha256")?),
                    "--version" => options.version = Some(value(&mut args, "--version")?),
                    "--size" => {
                        options.size = value(&mut args, "--size")?
                            .parse()
                            .map_err(|_| "--size 需要字节数")?;
                    }
                    "--wait-pid" => {
                        options.wait_pid = Some(
                            value(&mut args, "--wait-pid")?
                                .parse()
                                .map_err(|_| "--wait-pid 需要进程号")?,
                        );
                    }
                    other => return Err(format!("未知参数: {other}")),
                }
            }
            Ok(options)
        }
    }

    fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
        args.next().ok_or_else(|| format!("{flag} 缺少参数"))
    }
}
