//! `lueying` 命令行入口。当前只有 `ext top`;T12 的开发回路沿用这一层。

use std::process::ExitCode;
use std::time::Duration;

use lueying_cli::{
    Invocation, USAGE, default_metrics_path, parse_args, read_metrics, render_json, render_table,
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
    let invocation = parse_args(args)?;
    let Invocation::Top(options) = invocation else {
        println!("{USAGE}");
        return Ok(());
    };
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
