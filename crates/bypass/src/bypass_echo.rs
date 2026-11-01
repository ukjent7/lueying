//! 旁路进程协议的测试替身:一行请求回一行应答。
//!
//! 正常方法回显参数;`pid` 回自己的进程号(常驻测试判进程是否换过);
//! `boom` 走 error 分支;`hang` 挂住不答,给宿主的超时即杀路径用。
//! E2E 用 `CARGO_BIN_EXE_bypass-echo` 拉起本进程。

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

fn main() {
    let stdin = std::io::stdin();
    for line in BufReader::new(stdin.lock()).lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            break;
        };
        let method = value["method"].as_str().unwrap_or_default().to_string();
        if method == "boom" {
            println!("{{\"error\":\"测试错误\"}}");
        } else if method == "hang" {
            std::thread::sleep(Duration::from_secs(30));
            break;
        } else if method == "pid" {
            println!("{{\"result\":{{\"pid\":{}}}}}", std::process::id());
        } else {
            println!("{{\"result\":{{\"echo\":{}}}}}", value["args"]);
        }
        let _ = std::io::stdout().flush();
    }
}
