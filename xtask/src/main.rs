//! 仓库任务:
//! - `pack`:把可执行文件与数据装进发布目录,生成清单、版本号与 Inno 安装包。
//! - `update-manifest`:生成签名更新清单 `lueying-manifest.json`(安装包
//!   大小、SHA-256 与多镜像)。
//! - `sign-manifest`:用 Ed25519 种子给清单签名,输出 `.sig`(客户端内置
//!   公钥验签)。
//!
//! 用法:`cargo run -p xtask -- pack --bin target/release --data data --out dist/lueying`
//! 用法:`cargo run -p xtask -- update-manifest --version 0.2.0 --name lueying-0.2.0-setup.exe --size N --sha256 <hex> --mirror <url> --out dist`
//! 用法:`cargo run -p xtask -- sign-manifest --manifest dist/lueying-manifest.json --key <种子文件>`
//! 用法:`cargo run -p xtask -- winget --version 0.2.0 --sha256 <setup哈希> --out dist/winget`

#[path = "pack.rs"]
mod pack;

#[path = "manifest.rs"]
mod manifest;

#[path = "winget.rs"]
mod winget;

enum Task {
    Pack(pack::PackArgs),
    UpdateManifest(manifest::UpdateManifestArgs),
    SignManifest(manifest::SignArgs),
    Winget(winget::WingetArgs),
}

fn main() {
    let mut task = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "pack" => task = Some(Task::Pack(pack::parse_pack(&mut it))),
            "update-manifest" => {
                task = Some(Task::UpdateManifest(manifest::parse_update_manifest(
                    &mut it,
                )))
            }
            "sign-manifest" => {
                task = Some(Task::SignManifest(manifest::parse_sign_manifest(&mut it)))
            }
            "winget" => task = Some(Task::Winget(winget::parse_winget(&mut it))),
            other => {
                eprintln!("未知子命令或参数: {other}");
                std::process::exit(2);
            }
        }
    }
    let result = match task {
        Some(Task::Pack(args)) => pack::pack(args),
        Some(Task::UpdateManifest(args)) => manifest::update_manifest(args),
        Some(Task::SignManifest(args)) => manifest::sign_manifest(args),
        Some(Task::Winget(args)) => winget::winget(args),
        None => {
            eprintln!(
                "用法: xtask pack ... | xtask update-manifest ... | xtask sign-manifest ... | xtask winget ..."
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
