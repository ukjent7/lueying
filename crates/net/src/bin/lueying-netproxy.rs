//! 扩展网络代理进程:一行请求、一行应答,由扩展宿主按需拉起。
//!
//! 输入是旁路进程同款 `{"method":"fetch","args":{...}}`;输出
//! `{"result":{...}}` 或 `{"error":"..."}`。协议与白名单实现在
//! `lueying_net::proxy`,本入口只负责标准输入输出。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        use std::io::BufRead;
        let mut line = String::new();
        if let Ok(read) = std::io::stdin().lock().read_line(&mut line) {
            if read > 0 {
                println!("{}", lueying_net::proxy::handle_line(line.trim_end()));
            }
        }
    }
    #[cfg(not(windows))]
    println!("lueying-netproxy 仅支持 Windows");
}
