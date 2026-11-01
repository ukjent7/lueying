//! 生成 winget-pkgs 的三份 manifest(1.6 版式),发版后提交到
//! microsoft/winget-pkgs 仓库的 PR 里。
//!
//! 安装器是 Inno exe:静默开关与安装器一致;哈希与字节数来自发布
//! 流水线的 setup.exe。包 id 用 GitHub 组织名:ukjent7.lueying。
//!
//! `main` 的 `#[path]` 子模块(src 保持扁平)。

use std::path::PathBuf;

pub(crate) struct WingetArgs {
    version: String,
    sha256: String,
    out: PathBuf,
}

pub(crate) fn parse_winget(it: &mut impl Iterator<Item = String>) -> WingetArgs {
    let mut args = WingetArgs {
        version: String::new(),
        sha256: String::new(),
        out: PathBuf::from("dist/winget"),
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => args.version = it.next().unwrap_or_default(),
            "--sha256" => args.sha256 = it.next().unwrap_or_default(),
            "--out" => args.out = it.next().unwrap_or(args.out),
            _ => {}
        }
    }
    args
}

pub(crate) fn winget(args: WingetArgs) -> Result<(), String> {
    let version = args.version.trim().to_string();
    if version.is_empty() {
        return Err("--version 需要发布版本号(不带 v 前缀)".into());
    }
    let sha256 = args.sha256.trim().to_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("--sha256 需要 setup.exe 的 64 位十六进制哈希".into());
    }
    let id = "ukjent7.lueying";
    let repo = "https://github.com/ukjent7/lueying";
    let tag = format!("v{version}");
    let installer_url = format!("{repo}/releases/download/{tag}/lueying-{version}-setup.exe");
    let files = [
        (
            format!("{id}.yaml"),
            format!(
                "PackageIdentifier: {id}\nPackageVersion: {version}\nDefaultLocale: zh-CN\nManifestType: version\nManifestVersion: 1.6.0\n"
            ),
        ),
        (
            format!("{id}.installer.yaml"),
            format!(
                "PackageIdentifier: {id}\nPackageVersion: {version}\nInstallers:\n  - Architecture: x64\n    InstallerLocale: zh-CN\n    InstallerType: inno\n    InstallerUrl: {installer_url}\n    InstallerSha256: {sha256}\n    InstallerSwitches:\n      Silent: /VERYSILENT /SUPPRESSMSGBOXES /NORESTART\n      SilentWithProgress: /SILENT\n      InstallLocation: /DIR=\"<INSTALLPATH>\"\n    Scope: machine\nManifestType: installer\nManifestVersion: 1.6.0\n"
            ),
        ),
        (
            format!("{id}.locale.zh-CN.yaml"),
            format!(
                "PackageIdentifier: {id}\nPackageVersion: {version}\nPackageLocale: zh-CN\nPublisher: ukjent7\nPublisherUrl: {repo}\nPackageName: 掠影\nPackageUrl: {repo}\nLicense: GPL-3.0-or-later\nLicenseUrl: {repo}/blob/main/LICENSE\nShortDescription: 现代中文拼音输入法,基于 TSF 与 Rust 构建\nDescription: 掠影(Lueying)是一款专为 Windows 打造的中文拼音输入法,高准确率解码、无焦点 GPU 渲染候选窗与沙箱化扩展生态;按键流与词库全部留在本机。\nTags:\n  - ime\n  - pinyin\n  - chinese\nManifestType: defaultLocale\nManifestVersion: 1.6.0\n"
            ),
        ),
    ];
    std::fs::create_dir_all(&args.out).map_err(|e| format!("建 {}: {e}", args.out.display()))?;
    for (name, content) in files {
        let path = args.out.join(&name);
        std::fs::write(&path, content).map_err(|e| format!("写 {}: {e}", path.display()))?;
        println!("已生成 {}", path.display());
    }
    println!(
        "下一步:fork microsoft/winget-pkgs,把 {} 三份文件放到 manifests/u/ukjent7/lueying/{version}/ 后开 PR",
        args.out.display()
    );
    Ok(())
}
