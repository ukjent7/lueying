//! 安装/卸载冒烟:`lueying-win-e2e install` 跑真实安装包、真实更新器与真实卸载器的全链路。
//!
//! 原为独立的 install-e2e 包,并入 win-e2e 后只换了入口,检查项不变。

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
const INNO_UNINSTALL_KEY: &str = r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall\{5F0C8C3E-9B7A-4E2D-9A6B-2C1D8E4F7A10}_is1";

pub fn run() {
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
    let out = std::fs::File::create(&out_path).map_err(|e| format!("创建{name}输出文件: {e}"))?;
    let err = std::fs::File::create(&err_path).map_err(|e| format!("创建{name}错误文件: {e}"))?;
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
            "lueying-ext-host.exe",
            "lueying-settings.exe",
            "version.txt",
            "licenses/README.md",
        ] {
            if !target.join(name).exists() {
                return Err(format!("安装目录缺少 {name}"));
            }
            checks.push(format!("文件 {name}"));
        }
        let dll64 = "lueying_tsf.dll";
        if !target.join(dll64).is_file() {
            return Err(format!("安装目录缺少 {dll64}"));
        }
        checks.push(format!("TSF DLL {dll64}"));

        if !reg_query(RUN_KEY, Some("Lueying")) {
            return Err("缺少 HKCU 自启动项".into());
        }
        checks.push("自启动项".into());
        if !reg_query(INNO_UNINSTALL_KEY, None) {
            return Err("缺少 Inno 卸载项".into());
        }
        checks.push("卸载项".into());
        let inproc64 = format!(r"HKCR\CLSID\{CLSID}\InprocServer32");
        let target_text = target.to_string_lossy().to_ascii_lowercase();
        let value = reg_read(&inproc64, Some(""))
            .ok_or_else(|| "缺少 x64 TSF CLSID InprocServer32".to_string())?;
        if !value.to_ascii_lowercase().contains(&target_text) {
            return Err(format!("x64 TSF 注册指向了错误路径: {value}"));
        }
        checks.push("x64 TSF 注册指向安装目录".into());
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
        update_installed(&payload, &target, &root, &dist, &local, &appdata, &trace)?;
        checks.push("更新器升级到 9.9.9".into());

        phase("卸载");
        uninstall(&target, &root, &local, &appdata, &trace)?;
        phase("等待卸载清理");
        // 卸载器逐文件删除后删目录。TSF DLL 常被 ctfmon 等宿主加载,
        // Inno 的 uninsrestartdelete 会把它排进重启删除队列,目录只能
        // 留到重启;其它任何残留都说明卸载不完整。
        let deadline = Instant::now() + Duration::from_secs(15);
        while target.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut reboot_pending = Vec::new();
        if target.exists() {
            let mut leftovers = Vec::new();
            collect_files(&target, &mut leftovers)?;
            let mut unexpected = Vec::new();
            for file in leftovers {
                let name = file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !is_tsf_dll_leftover(&name) {
                    unexpected.push(file.display().to_string());
                    continue;
                }
                // 宿主已经放开的文件直接删掉;仍被占用的必须确认
                // Inno 已安排重启删除,否则就是卸载器漏删。
                if std::fs::remove_file(&file).is_ok() {
                    continue;
                }
                if !pending_delete(&file, &root.join("uninstall.log")) {
                    return Err(format!(
                        "卸载后 {} 仍在,且没有安排重启删除。重启删除队列现状:\n{}",
                        file.display(),
                        pending_queue_dump()
                    ));
                }
                reboot_pending.push(name);
            }
            if !unexpected.is_empty() {
                return Err(format!(
                    "卸载后目录仍有非 TSF-DLL 残留: {}",
                    unexpected.join(", ")
                ));
            }
            if reboot_pending.is_empty() {
                let _ = std::fs::remove_dir_all(&target);
                if target.exists() {
                    return Err(format!("卸载后目录仍在: {}", target.display()));
                }
            }
        }
        if reg_query(INNO_UNINSTALL_KEY, None) {
            return Err("卸载后卸载项仍在".into());
        }
        if reg_query(RUN_KEY, Some("Lueying")) {
            return Err("卸载后自启动项仍在".into());
        }
        if reg_query(&inproc64, Some("")) {
            return Err("卸载后 TSF CLSID 仍在".into());
        }
        // profile 已注销时查询本身可能失败;只有明确仍启用才算清理失败。
        if profile_enabled().unwrap_or(false) {
            return Err("卸载后「掠影」仍在当前用户输入法列表".into());
        }
        checks.push("注册项与输入法列表已清理".into());
        if !reboot_pending.is_empty() {
            checks.push(format!(
                "TSF DLL 被宿主加载,已安排重启删除: {}",
                reboot_pending.join(", ")
            ));
        }
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
        "lueying-ext-host.exe",
        "lueying-settings.exe",
        "lueying_tsf.dll",
    ] {
        std::fs::copy(bin.join(name), payload.join(name))
            .map_err(|e| format!("复制 {name}: {e}"))?;
    }
    std::fs::write(payload.join("version.txt"), version)
        .map_err(|e| format!("写 version.txt: {e}"))?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let license = root.join("LICENSE");
    std::fs::copy(&license, payload.join("LICENSE")).map_err(|e| format!("复制 LICENSE: {e}"))?;
    let licenses = payload.join("licenses");
    std::fs::create_dir_all(&licenses).map_err(|e| format!("创建 licenses: {e}"))?;
    std::fs::write(licenses.join("README.md"), "第三方许可见此处。\n")
        .map_err(|e| format!("写 licenses/README.md: {e}"))?;
    Ok(())
}

fn compile_setup(payload: &Path, version: &str, out: &Path) -> Result<PathBuf, String> {
    let iscc = iscc_path()?;
    std::fs::create_dir_all(out).map_err(|e| format!("创建 {}: {e}", out.display()))?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../installer/lueying.iss");
    let setup = out.join(format!("lueying-{version}-setup.exe"));
    let _ = std::fs::remove_file(&setup);
    let mut command = Command::new(iscc);
    command
        .arg(format!("/DPayloadDir={}", payload.display()))
        .arg(format!("/DOutDir={}", out.display()))
        .arg(format!("/DVersion={version}"))
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
    // 中文语言文件在部分 runner 上初始化失败会让 Setup 直接以 1 退出,
    // 且不写 /LOG。英文是 Inno 自带语言,拿它再试一次;两次都失败才报错。
    let mut failures = Vec::new();
    for lang in ["chinesesimplified", "english"] {
        let label = format!("{name}({lang})");
        let mut command = Command::new(setup);
        command
            .arg("/VERYSILENT")
            .arg("/SUPPRESSMSGBOXES")
            .arg("/NORESTART")
            // 与 winget 的静默开关一致:静默升级不打扰运行中的应用,
            // 被宿主锁住的 TSF 组件由 BeforeInstall 的改名让路处理,
            // 不靠 Restart Manager 强杀进程。
            .arg("/NOCLOSEAPPLICATIONS")
            .arg(format!("/LANG={lang}"))
            .arg(format!("/DIR={}", target.display()))
            .arg(format!("/LOG={}", log.display()));
        apply_env(&mut command, local, appdata, target, trace);
        let output = run_command(&label, &mut command, Duration::from_secs(150))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = std::fs::read(log)
            .map(|bytes| tail(&bytes))
            .unwrap_or_default();
        failures.push(format!(
            "{label} 退出码 {:?}: {detail}",
            output.status.code()
        ));
    }
    Err(failures.join("\n"))
}

/// 用本地 9.9.9 安装包跑一遍真实升级:静默运行 Inno 安装包替换版本。
/// 设置窗的更新流程就是「下载到临时目录后运行安装包」,这里验安装器
/// 侧的升级覆盖与 --prune。
#[allow(clippy::too_many_arguments)]
fn update_installed(
    payload: &Path,
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
        "lueying-ext-host.exe",
        "lueying-settings.exe",
        "lueying_tsf.dll",
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

    // 被宿主加载的旧 DLL 允许留到下次,所以用未注册的合成文件验证 --prune。
    let stale = ["lueying_tsf-0.0.1.dll"];
    for name in stale {
        std::fs::write(target.join(name), b"stale").map_err(|e| format!("写旧 DLL {name}: {e}"))?;
    }

    phase("编译 9.9.9 安装包");
    let setup = compile_setup(&package, "9.9.9", dist)?;

    // 模拟宿主:像 Chrome 一样加载着旧 TSF 组件。静默安装带
    // /NOCLOSEAPPLICATIONS(与 winget 一致),Restart Manager 不强杀
    // 占用进程,升级靠改名让路完成:宿主进程与其加载的模块照常运行,
    // 新进程从原名拿到新文件。LoadLibrary 的结果写进状态文件:加载失败
    // 会让升级退化成普通覆盖,场景失真,必须当场报错,不能让断言空转。
    let holder_script = root.join("tsf-holder.ps1");
    let holder_status = root.join("tsf-holder-loaded.txt");
    std::fs::write(
        &holder_script,
        "Add-Type -Namespace W -Name K -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern IntPtr LoadLibrary(string path);'\n$h = [W.K]::LoadLibrary($args[0])\nSet-Content -Path $args[1] -Value $h -Encoding ascii\nStart-Sleep -Seconds 600\n",
    )
    .map_err(|e| format!("写 holder 脚本: {e}"))?;
    let mut holder = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&holder_script)
        .arg(target.join("lueying_tsf.dll"))
        .arg(&holder_status)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 DLL 宿主: {e}"))?;

    // 等 holder 加载完再跑升级,不然安装可能赶在加载完成之前复制文件。
    let deadline = Instant::now() + Duration::from_secs(30);
    let loaded = loop {
        match std::fs::read_to_string(&holder_status) {
            Ok(text) => {
                let handle = text.trim();
                break !handle.is_empty() && handle != "0";
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break false,
        }
    };
    if !loaded {
        let _ = holder.kill();
        return Err("模拟宿主的进程没有加载到旧 TSF 组件".into());
    }

    run_setup(
        "升级安装",
        &setup,
        &target,
        &root.join("update.log"),
        local,
        appdata,
        trace,
    )?;

    let installed = std::fs::read_to_string(target.join("version.txt")).unwrap_or_default();
    if installed.trim() != "9.9.9" {
        return Err(format!("更新后版本是 {},期望 9.9.9", installed.trim()));
    }
    if !target.join("lueying_tsf.dll").is_file() {
        return Err("更新后缺少 lueying_tsf.dll".into());
    }
    for name in stale {
        if target.join(name).exists() {
            return Err(format!("更新后仍留着旧 DLL {name}"));
        }
    }
    // 改名让路生效的证据:宿主进程还在跑,新组件已从原名就位——若仍走
    // 覆盖旧文件的路线,Setup 会因文件被宿主占用而失败。.previous 的去留
    // 取决于系统对被加载模块的删除语义:允许删除就当场消失,不允许则留给
    // 下次安装的 [InstallDelete];两种结果都算升级成功,不作断言。
    if holder.try_wait().map_or(false, |state| state.is_some()) {
        let _ = holder.kill();
        return Err("升级时持有旧 TSF 组件的进程被意外终止".into());
    }
    let _ = holder.kill();
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
        .arg(format!("/LOG={}", log.display()));
    apply_env(&mut command, local, appdata, target, trace);
    let output = run_command("卸载器", &mut command, Duration::from_secs(120))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = std::fs::read(&log)
            .map(|bytes| tail(&bytes))
            .unwrap_or_default();
        Err(format!("卸载器退出码 {:?}: {detail}", output.status.code()))
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

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(800);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

/// 递归列出目录下的文件;目录本身不作为残留,空目录可以整体删除。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("读取 {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取 {}: {e}", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("查看 {}: {e}", path.display()))?;
        if kind.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// 卸载时允许因宿主仍加载而留到重启的 TSF 组件:稳定名本体、升级时
/// 改名让路的 .previous,以及旧版本安装留下的版本化文件。
fn is_tsf_dll_leftover(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // 与安装辅助程序的 prune 同一套前缀后缀:稳定名、版本化遗留,
    // 以及带时间戳的改名让路文件 lueying_tsf-<时间戳>.dll.previous。
    lower.starts_with("lueying_tsf")
        && (lower.ends_with(".dll") || lower.ends_with(".dll.previous"))
}

/// 文件是否已排进 Windows 的重启删除队列。Inno 对 `uninsrestartdelete`
/// 文件调用 MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT);队列过长时系统会
/// 溢出到 `PendingFileRenameOperations2`,两处都要查。卸载日志里的
/// 「Will delete on restart」是同一动作的兜底证据。
fn pending_delete(path: &Path, uninstall_log: &Path) -> bool {
    let needle = path.to_string_lossy().to_ascii_lowercase();
    // 重启删除队列存的是长名,而安装目录可能以 8.3 短名(RUNNER~1)给出;
    // 展开成长名再比一次。卸载日志用的是安装时的写法,仍按原样比。
    let long = std::fs::canonicalize(path)
        .map(|path| {
            path.to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_ascii_lowercase()
        })
        .unwrap_or_else(|_| needle.clone());
    [
        "PendingFileRenameOperations",
        "PendingFileRenameOperations2",
    ]
    .iter()
    .any(|value| registry_value_contains(value, &needle) || registry_value_contains(value, &long))
        || log_schedules_delete(uninstall_log, &needle)
}

fn registry_value_contains(value: &str, needle: &str) -> bool {
    reg_query_value(value).is_some_and(|text| text.to_ascii_lowercase().contains(needle))
}

/// 读重启删除队列的原始输出;失败诊断用,读不到就注明。
fn reg_query_value(value: &str) -> Option<String> {
    let mut command = Command::new("reg");
    command.args([
        "query",
        r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager",
        "/v",
        value,
    ]);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 断言失败时把两个队列值都打出来:路径形式(短名/长名/`\??\` 前缀)与
/// 队列是否被后续写入覆盖,只有原文能分辨。
fn pending_queue_dump() -> String {
    let mut text = String::new();
    for value in [
        "PendingFileRenameOperations",
        "PendingFileRenameOperations2",
    ] {
        text.push_str(&format!(
            "--- {value} ---\n{}\n",
            reg_query_value(value).unwrap_or_else(|| "(读不到)".into())
        ));
    }
    text
}

fn log_schedules_delete(log: &Path, needle: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(log) else {
        return false;
    };
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some((_, rest)) = line.split_once("Deleting file: ") else {
            continue;
        };
        if rest.trim().to_ascii_lowercase() != needle {
            continue;
        }
        if lines
            .next()
            .is_some_and(|next| next.contains("Will delete on restart"))
        {
            return true;
        }
    }
    false
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
        let enabled =
            unsafe { profiles.IsEnabledLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE) }
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
