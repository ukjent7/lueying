//! `lueying` 命令行入口:扩展开发回路(new/pack/install/reload/logs/replay)与运行指标(top)。

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use lueying_cli::{
    Invocation, SceneReplayArgs, TopOptions, USAGE, default_metrics_path, extcmd, parse_args,
    read_metrics, render_json, render_table, replay, scenecmd,
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
        Invocation::SceneReplay(args) => scene_replay(&args)?,
        Invocation::ScenePack(args) => {
            let path = scenecmd::pack(&args)?;
            println!(
                "已打包 {};下一步:lueying scene install {}",
                path.display(),
                path.display()
            );
        }
        Invocation::SceneInstall(args) => {
            let dir = scenecmd::install(&args)?;
            println!("已安装到 {};场景一秒内热重载生效", dir.display());
        }
    }
    Ok(())
}

/// 校验场景文件;目录就扫全部 .toml,逐个报问题。
fn scene_check(path: &Path) -> Result<(), String> {
    let files: Vec<std::path::PathBuf> = if path.is_dir() {
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
        let text = std::fs::read_to_string(file)
            .map_err(|error| format!("读 {}: {error}", file.display()))?;
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

fn scene_replay(args: &SceneReplayArgs) -> Result<(), String> {
    let script_text = std::fs::read_to_string(&args.script)
        .map_err(|error| format!("读 {}: {error}", args.script.display()))?;
    let script = lueying_scene::replay::parse(&args.script.display().to_string(), &script_text)
        .map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    let scene_path = args
        .scene
        .clone()
        .unwrap_or_else(|| args.script.with_file_name("scene.toml"));
    let scene_text = std::fs::read_to_string(&scene_path).map_err(|error| {
        format!(
            "读 {}: {error};场景不在脚本旁边时用 --scene 指定",
            scene_path.display()
        )
    })?;
    let scene =
        lueying_scene::parse(&scene_path.display().to_string(), &scene_text).map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })?;
    let frames = lueying_scene::replay(&script, scene);
    if args.json {
        let json = serde_json::to_string(&frames).map_err(|error| error.to_string())?;
        println!("{json}");
        return Ok(());
    }
    for (index, frame) in frames.iter().enumerate() {
        let below = frame
            .items
            .iter()
            .filter(|item| item.layer == lueying_scene::Layer::BelowText)
            .count();
        let above = frame.items.len() - below;
        let particles = frame
            .items
            .iter()
            .filter(|item| matches!(item.body, lueying_scene::Body::Particles { .. }))
            .count();
        let degraded = if frame.degraded { ",已降级" } else { "" };
        println!(
            "帧 {index}:图元 {}(下 {} / 上 {}),粒子 {particles}{degraded}",
            frame.items.len(),
            below,
            above,
        );
    }
    println!("共 {} 帧", frames.len());
    Ok(())
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
