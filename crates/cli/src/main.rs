//! `lueying` 命令行入口:扩展开发回路(new/pack/install/reload/logs/replay)与运行指标(top)。

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use lueying_cli::{
    Invocation, TopOptions, USAGE, default_metrics_path, extcmd, parse_args, read_metrics,
    render_json, render_table, replay,
};

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run(args: impl IntoIterator<Item = String>) -> Result<(), String> {
    match parse_args(args)? {
        Invocation::Help => println!("{USAGE}"),
        Invocation::New(args) => {
            let dir = extcmd::new(&args)?;
            println!(
                "已创建 {};编出 wasm32 后把 {{}}.wasm 拷成 extension.wasm,再 lueying ext pack {}",
                dir.display(),
                dir.display()
            );
        }
        Invocation::Pack(args) => {
            let path = extcmd::pack(&args)?;
            println!(
                "已打包 {};下一步:lueying ext install {}",
                path.display(),
                path.display()
            );
        }
        Invocation::Install(args) => {
            let dir = extcmd::install(&args)?;
            println!("已安装 {};启用后 lueying ext reload 生效", dir.display());
        }
        Invocation::Reload => {
            extcmd::reload()?;
            println!("已重载");
        }
        Invocation::Logs(args) => println!("{}", extcmd::logs(&args)?),
        Invocation::Replay(args) => print!("{}", replay::run(&args)?),
        Invocation::Commit(text) => println!("{}", extcmd::commit(&text)?),
        Invocation::Run(id) => println!("{}", extcmd::run(&id)?),
        Invocation::Commands => {
            let entries = extcmd::commands()?;
            if entries.is_empty() {
                println!("没有可触发的命令;先安装并启用扩展,再 lueying ext reload");
            }
            for (id, title) in entries {
                println!("{id:<40}{title}");
            }
        }
        Invocation::Top(options) => top(options)?,
        Invocation::SceneCheck(path) => scene_check(&path)?,
        Invocation::SceneSchema => print!("{}", lueying_scene::SCHEMA),
    }
    Ok(())
}

/// 校验场景文件;目录就扫全部 .toml,逐个报问题。
fn scene_check(path: &Path) -> Result<(), String> {
    let mut files: Vec<std::path::PathBuf> = if path.is_dir() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(path)
            .map_err(|error| format!("读 {}: {error}", path.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort();
        files
    } else {
        vec![path.to_owned()]
    };
    if files.is_empty() {
        return Err(format!("{} 里没有 .toml 场景文件", path.display()));
    }
    let mut failed = Vec::new();
    for file in &files {
        let text =
            std::fs::read_to_string(file).map_err(|error| format!("读 {}: {error}", file.display()))?;
        match lueying_scene::parse(&file.display().to_string(), &text) {
            Ok(_) => println!("{} 合法", file.display()),
            Err(errors) => failed.extend(errors.iter().map(|error| error.to_string())),
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n"))
    }
}

fn top(options: TopOptions) -> Result<(), String> {
    let path = options
        .file
        .or_else(default_metrics_path)
        .ok_or("找不到指标快照路径;用 --file 指定")?;
    loop {
        let metrics = read_metrics(&path)?;
        if options.json {
            println!("{}", render_json(&metrics)?);
        } else {
            print!("{}", render_table(&metrics));
        }
        if !options.watch {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
        if !options.json {
            println!();
        }
    }
}
