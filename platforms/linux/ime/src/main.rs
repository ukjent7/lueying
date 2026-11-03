//! 掠影 Linux 输入法入口。

fn main() {
    if let Err(error) = lueying_linux::run() {
        eprintln!("lueying-linux: {error}");
        std::process::exit(1);
    }
}
