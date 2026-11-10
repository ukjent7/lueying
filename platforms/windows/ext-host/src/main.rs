//! 掠影扩展宿主进程入口:`lueying-ext-host <数据目录> <本地数据目录>`。
//!
//! 标准输入输出是与服务端的协议通道,诊断只能写标准错误(服务端把它接到日志)。
//! `--selftest <报告>` 跑一次组件自检后退出,供 Windows 端到端验证。

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.as_slice() {
        [flag, report] if flag == "--selftest" => lueying_ext_host::selftest(report.as_ref()),
        [data, local] => lueying_ext_host::run(lueying_ext_host::Paths {
            data: data.into(),
            local: local.into(),
        }),
        _ => {
            eprintln!("用法: lueying-ext-host <数据目录> <本地数据目录> | --selftest <报告>");
            2
        }
    };
    std::process::exit(code);
}
