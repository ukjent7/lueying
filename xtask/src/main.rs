//! 仓库任务:
//! - `dict`:把白霜拼音(Rime dict.yaml)转成本项目的 TSV/二进制词典。
//! - `pack`:把可执行文件与数据装进发布目录,生成清单、版本号与 Inno 安装包。
//! - `update-manifest`:生成签名更新清单 `lueying-manifest.json`(安装包
//!   大小、SHA-256 与多镜像)。
//! - `sign-manifest`:用 Ed25519 种子给清单签名,输出 `.sig`(客户端内置
//!   公钥验签)。
//!
//! 用法:`cargo run -p xtask -- dict --frost <白霜cn_dicts目录> --out <输出.tsv>`
//! 用法:`cargo run -p xtask -- pack --bin target/release --bin32 target/tsf/i686-pc-windows-msvc/release --data data --out dist/lueying`
//! 用法:`cargo run -p xtask -- update-manifest --version 0.2.0 --name lueying-0.2.0-setup.exe --size N --sha256 <hex> --mirror <url> --out dist`
//! 用法:`cargo run -p xtask -- sign-manifest --manifest dist/lueying-manifest.json --key <种子文件>`

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use lueying_dict::syllables::is_syllable;

struct Args {
    frost: PathBuf,
    out: PathBuf,
    /// 另行物化的二进制词典(运行期 mmap 零解析加载)。
    bin: Option<PathBuf>,
}

struct PackArgs {
    bin: PathBuf,
    /// i686 构建目录;提供时打包 32 位 TSF DLL。
    bin32: Option<PathBuf>,
    data: PathBuf,
    out: PathBuf,
    version: String,
    allow_missing_data: bool,
}

enum Task {
    Dict(Args),
    Pack(PackArgs),
    UpdateManifest(UpdateManifestArgs),
    SignManifest(SignArgs),
}

fn main() {
    let mut task = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "dict" => task = Some(Task::Dict(parse_dict(&mut it))),
            "pack" => task = Some(Task::Pack(parse_pack(&mut it))),
            "update-manifest" => task = Some(Task::UpdateManifest(parse_update_manifest(&mut it))),
            "sign-manifest" => task = Some(Task::SignManifest(parse_sign_manifest(&mut it))),
            other => {
                eprintln!("未知子命令或参数: {other}");
                std::process::exit(2);
            }
        }
    }
    let result = match task {
        Some(Task::Dict(args)) => run(args),
        Some(Task::Pack(args)) => pack(args),
        Some(Task::UpdateManifest(args)) => update_manifest(args),
        Some(Task::SignManifest(args)) => sign_manifest(args),
        None => {
            eprintln!(
                "用法: xtask dict ... | xtask pack ... | xtask update-manifest ... | xtask sign-manifest ..."
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn parse_dict(it: &mut impl Iterator<Item = String>) -> Args {
    let mut frost = None;
    let mut out = None;
    let mut bin = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frost" => frost = it.next().map(PathBuf::from),
            "--out" => out = it.next().map(PathBuf::from),
            "--bin" => bin = it.next().map(PathBuf::from),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    Args {
        frost: frost.expect("--frost 需要 <白霜 cn_dicts 目录>"),
        out: out.expect("--out 需要 <输出文件>"),
        bin,
    }
}

fn parse_pack(it: &mut impl Iterator<Item = String>) -> PackArgs {
    let mut args = PackArgs {
        bin: PathBuf::from("target/release"),
        bin32: None,
        data: PathBuf::from("data"),
        out: PathBuf::from("dist/lueying"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        allow_missing_data: false,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bin" => args.bin = it.next().map(PathBuf::from).unwrap_or(args.bin),
            "--bin32" => args.bin32 = it.next().map(PathBuf::from),
            "--data" => args.data = it.next().map(PathBuf::from).unwrap_or(args.data),
            "--out" => args.out = it.next().map(PathBuf::from).unwrap_or(args.out),
            "--version" => args.version = it.next().unwrap_or(args.version),
            "--allow-missing-data" => args.allow_missing_data = true,
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

struct UpdateManifestArgs {
    version: String,
    name: String,
    size: u64,
    sha256: String,
    mirrors: Vec<String>,
    out: PathBuf,
}

fn parse_update_manifest(it: &mut impl Iterator<Item = String>) -> UpdateManifestArgs {
    let mut args = UpdateManifestArgs {
        version: env!("CARGO_PKG_VERSION").to_string(),
        name: String::new(),
        size: 0,
        sha256: String::new(),
        mirrors: Vec::new(),
        out: PathBuf::from("dist"),
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => args.version = it.next().unwrap_or(args.version),
            "--name" => args.name = it.next().unwrap_or(args.name),
            "--size" => {
                args.size = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(args.size);
            }
            "--sha256" => args.sha256 = it.next().unwrap_or(args.sha256),
            "--mirror" => {
                if let Some(url) = it.next() {
                    args.mirrors.push(url);
                }
            }
            "--out" => args.out = it.next().map(PathBuf::from).unwrap_or(args.out),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

struct SignArgs {
    manifest: PathBuf,
    key: Option<PathBuf>,
    key_env: Option<String>,
}

fn parse_sign_manifest(it: &mut impl Iterator<Item = String>) -> SignArgs {
    let mut args = SignArgs {
        manifest: PathBuf::from("dist/lueying-manifest.json"),
        key: None,
        key_env: None,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--manifest" => args.manifest = it.next().map(PathBuf::from).unwrap_or(args.manifest),
            "--key" => args.key = it.next().map(PathBuf::from),
            "--key-env" => args.key_env = it.next(),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

/// 生成签名更新清单:只描述这一个安装包的大小、哈希与镜像;发布说明仍以
/// GitHub Release 正文为准,清单里不重复。
fn update_manifest(args: UpdateManifestArgs) -> Result<(), String> {
    if !is_sha256_hex(&args.sha256) {
        return Err("--sha256 需要 64 位十六进制".into());
    }
    if args.mirrors.is_empty() {
        return Err("--mirror 至少一条下载地址".into());
    }
    if args.mirrors.iter().any(|url| !url.starts_with("https://")) {
        return Err("--mirror 只接受 https 地址".into());
    }
    let manifest = serde_json::json!({
        "version": 1,
        "release": args.version,
        "files": [{
            "name": args.name,
            "size": args.size,
            "sha256": args.sha256,
            "mirrors": args.mirrors,
        }],
    });
    let mut text = serde_json::to_string_pretty(&manifest).map_err(|e| format!("生成清单: {e}"))?;
    text.push('\n');
    let path = args.out.join("lueying-manifest.json");
    std::fs::write(&path, &text).map_err(|e| format!("写 {}: {e}", path.display()))?;
    println!(
        "清单 {} 已生成;用 sign-manifest 签名后随 Release 上传",
        path.display()
    );
    Ok(())
}

/// 用 Ed25519 种子给清单签名,输出 `.sig`(128 位十六进制,清单字节原文的
/// 签名);签完立即用内置流程自检,并回显公钥指纹供人工与代码常量核对。
fn sign_manifest(args: SignArgs) -> Result<(), String> {
    use ed25519_dalek::{Signer, SigningKey, Verifier};
    let seed_hex = match (&args.key, &args.key_env) {
        (Some(path), _) => {
            std::fs::read_to_string(path).map_err(|e| format!("读 {}: {e}", path.display()))?
        }
        (None, Some(name)) => std::env::var(name).map_err(|_| format!("环境变量 {name} 未设置"))?,
        (None, None) => {
            return Err("用 --key <种子文件> 或 --key-env <变量名> 提供签名种子".into());
        }
    };
    let seed = decode_seed(seed_hex.trim())?;
    let key = SigningKey::from_bytes(&seed);
    let bytes = std::fs::read(&args.manifest)
        .map_err(|e| format!("读 {}: {e}", args.manifest.display()))?;
    let signature = key.sign(&bytes);
    key.verifying_key()
        .verify(&bytes, &signature)
        .map_err(|e| format!("签名自检失败: {e}"))?;
    let name = args
        .manifest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("lueying-manifest.json");
    let sig_path = args.manifest.with_file_name(format!("{name}.sig"));
    let mut text = hex(&signature.to_bytes());
    text.push('\n');
    std::fs::write(&sig_path, &text).map_err(|e| format!("写 {}: {e}", sig_path.display()))?;
    println!(
        "签名 {} 已生成;公钥指纹 {}",
        sig_path.display(),
        hex(&key.verifying_key().to_bytes())
    );
    Ok(())
}

fn decode_seed(text: &str) -> Result<[u8; 32], String> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("签名种子要 64 位十六进制(Ed25519 私钥种子)".into());
    }
    let mut seed = [0u8; 32];
    for (index, slot) in seed.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| "签名种子要 64 位十六进制".to_string())?;
    }
    Ok(seed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 发布目录的必需文件。DLL 文件名随 Cargo 包名生成,两种拼写都接受。
const PACK_BINS: &[&str] = &[
    "lueying-server.exe",
    "lueying-settings.exe",
    "lueying-user.exe",
    "lueying-update.exe",
];
const PACK_DLLS: &[&str] = &["lueying_tsf.dll", "lueying-tsf.dll"];
/// 默认包带精确词典、两档重排模型与精简语法模型。`main.gram` 缺失时引擎
/// 退回纯词频;发布包由 `tools/fetch_gram.sh` 钉版本,文件名保持 `main.gram`
/// 让 Server 无需配置就能发现它。
const PACK_DATA: &[&str] = &[
    "main.pyd",
    "main.gram",
    "rerank-4m.safetensors",
    "rerank-25m.safetensors",
];
/// 可选 OCR 引擎的模型;由 `tools/fetch_ppocr.sh` 拉到 `data/ppocr/`。
const PACK_PPOCR: &[&str] = &["det.onnx", "rec.onnx", "dict.txt"];

fn pack(args: PackArgs) -> Result<(), String> {
    // 只清理自己生成的目录(有 manifest),避免误删用户给的其他路径。
    if args.out.exists() {
        let manifest = args.out.join("manifest.json");
        if !manifest.exists() {
            return Err(format!(
                "{} 已存在且不是发布目录;请换 --out 或先删除它",
                args.out.display()
            ));
        }
        std::fs::remove_dir_all(&args.out)
            .map_err(|e| format!("清理 {}: {e}", args.out.display()))?;
    }
    std::fs::create_dir_all(&args.out).map_err(|e| format!("创建 {}: {e}", args.out.display()))?;

    let mut files = Vec::new();
    for name in PACK_BINS {
        copy_file(&args.bin.join(name), &args.out, name, &mut files)?;
    }
    let dll = PACK_DLLS
        .iter()
        .map(|name| args.bin.join(name))
        .find(|path| path.exists())
        .ok_or_else(|| format!("{} 里缺少 lueying_tsf.dll", args.bin.display()))?;
    // 版本号由 Inno 在安装时写进目标文件名(见 installer/lueying.iss),
    // 发布目录保持构建产物的原名,便于 E2E 直接复用。
    copy_file(&dll, &args.out, "lueying_tsf.dll", &mut files)?;
    if let Some(bin32) = &args.bin32 {
        let dll32 = PACK_DLLS
            .iter()
            .map(|name| bin32.join(name))
            .find(|path| path.exists())
            .ok_or_else(|| format!("{} 里缺少 32 位 lueying_tsf.dll", bin32.display()))?;
        copy_file(&dll32, &args.out, "lueying_tsf32.dll", &mut files)?;
    } else {
        println!("警告: 未提供 --bin32,发布包只含 x64 TSF DLL");
    }

    for name in PACK_DATA {
        // fetch_rerank.sh 默认下载到 data/rerank/,发布目录却是平铺的;两种布局都接受。
        let flat = args.data.join(name);
        let source = if flat.exists() {
            flat
        } else {
            args.data.join("rerank").join(name)
        };
        if source.exists() {
            copy_file(&source, &args.out, name, &mut files)?;
        } else if args.allow_missing_data {
            println!("警告: 缺少数据文件 {name},跳过");
        } else {
            return Err(format!("缺少数据文件 {}", source.display()));
        }
    }
    // PP-OCRv6 是可选引擎;模型在就随包,缺了只警告(默认系统 OCR 不受影响)。
    let ppocr = args.data.join("ppocr");
    if ppocr.is_dir() {
        let target = args.out.join("models/ppocr");
        std::fs::create_dir_all(&target).map_err(|e| format!("创建 {}: {e}", target.display()))?;
        for name in PACK_PPOCR {
            let source = ppocr.join(name);
            if source.exists() {
                copy_file(
                    &source,
                    &args.out,
                    &format!("models/ppocr/{name}"),
                    &mut files,
                )?;
            } else if args.allow_missing_data {
                println!("警告: 缺少 PP-OCR 文件 {}", source.display());
            } else {
                return Err(format!("缺少 PP-OCR 文件 {}", source.display()));
            }
        }
    } else {
        println!("警告: 没有 PP-OCR 模型目录 {},跳过", ppocr.display());
    }
    // TSV 是二进制不可用时的兼容回退,可选。
    if args.data.join("main.tsv").exists() {
        copy_file(
            &args.data.join("main.tsv"),
            &args.out,
            "main.tsv",
            &mut files,
        )?;
    }

    // 许可与第三方署名随二进制分发。主程序 GPL 放根目录;LM 运行时与两档
    // 重排模型是 Apache-2.0,放 licenses/,避免安装包里只有可执行文件。
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or("找不到仓库根目录")?;
    let license = root.join("LICENSE");
    if license.exists() {
        copy_file(&license, &args.out, "LICENSE", &mut files)?;
    }
    let licenses = args.out.join("licenses");
    std::fs::create_dir_all(&licenses).map_err(|e| format!("创建 {}: {e}", licenses.display()))?;
    let lm_license = root.join("crates/lm/LICENSE");
    if lm_license.exists() {
        copy_file(
            &lm_license,
            &args.out,
            "licenses/Apache-2.0.txt",
            &mut files,
        )?;
    }
    let cc_license = root.join("licenses/CC-BY-4.0.txt");
    copy_file(&cc_license, &args.out, "licenses/CC-BY-4.0.txt", &mut files)?;
    let notice = "\
# 随包第三方许可与署名

- 白霜拼音词库:GPL-3.0,https://github.com/gaboolic/rime-frost,\
commit fbdf9efc4a5036db5f37b97323e1b936e8fe993c。
- 青简常用词表与词频:GPL-3.0-or-later,https://github.com/qingjian-team/qingjian,\
版本 0.1.1;随包词典用这份常用词表补入白霜未收录的词,并给多字常用词提频。\
词频语料来源包括中文维基百科(CC BY-SA 4.0)、LCCC(MIT)与 THUOCL(MIT),\
详见青简词库头部说明。
- 万象拼音语法模型子集:CC-BY-4.0,作者 amzxyz,\
https://github.com/amzxyz/rime-wanxiang-data (LTS);\
许可 https://creativecommons.org/licenses/by/4.0/;\
完整模型 sha256 9eedac1d836f3fdafb61b162035c8937253aedc106be40b2b957d9ab64e9ed89。\
随包的 main.gram 是 WindInput(https://github.com/huanfeng/WindInput,\
commit a99a7ad35701ff87dac6bd2057b0f594cd004d81)按真实输入语料裁剪出的 5.3MB 子集,\
未再修改;sha256 c85801930ab5703d28472f86153f37a367287156ef70a523c5a4fb07f6a95147。\
许可全文见 CC-BY-4.0.txt。
- chinese-ime-lm:Apache-2.0,https://github.com/metasequoiaime/chinese-ime-lm,\
commit b97e99cbaa4a1a358158d717ffd434c472d5d5ca;全文见 Apache-2.0.txt。
- pinyin-ime-reranker-4M:Apache-2.0,\
https://huggingface.co/metasequoiaime/pinyin-ime-reranker-4M,\
revision e5b1f7e768d7cb2b6ff334db4e34af153920c6ff。
- pinyin-ime-reranker-25M:Apache-2.0,\
https://huggingface.co/metasequoiaime/pinyin-ime-reranker-25M,\
revision 75295b373a7ef0d14e9bebd32c1625c9cfcfea49。
- PP-OCRv6 small det/rec:Apache-2.0,\
https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx (revision \
28fe5895c24fd108c19eb3e8479f4ab385fbfc62)、\
https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx (revision \
b8f84f0b80c529de40b4fbb3544b84fa7233a513);\
词表从 rec 仓库的 inference.yml 提取,未修改模型;全文见 Apache-2.0.txt。
";
    std::fs::write(licenses.join("README.md"), notice)
        .map_err(|e| format!("写 licenses/README.md: {e}"))?;
    files.push(("licenses/README.md".into(), notice.len() as u64));

    // 官方起步扩展随安装包分发(几十 KB 的 JS);首次安装复制到当前用户目录。
    let extensions = root.join("extensions");
    if extensions.is_dir() {
        copy_tree(
            &extensions,
            &args.out.join("extensions"),
            "extensions",
            &mut files,
        )?;
    }

    std::fs::write(args.out.join("version.txt"), format!("{}\n", args.version))
        .map_err(|e| format!("写 version.txt: {e}"))?;
    let manifest = serde_json::json!({
        "name": "lueying",
        "version": args.version,
        "files": files.iter().map(|(name, bytes)| {
            serde_json::json!({ "path": name, "bytes": bytes })
        }).collect::<Vec<_>>(),
    });
    std::fs::write(
        args.out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("生成清单: {e}"))?,
    )
    .map_err(|e| format!("写 manifest.json: {e}"))?;

    let total: u64 = files.iter().map(|(_, bytes)| *bytes).sum();
    println!(
        "发布目录 {}: {} 个文件, {:.1} MiB",
        args.out.display(),
        files.len() + 2,
        total as f64 / (1024.0 * 1024.0)
    );
    compile_setup(&args, &root)?;
    Ok(())
}

/// 调 ISCC 把发布目录打成 Inno Setup 安装包。输出在发布目录的父目录,
/// 文件名与 CI 的发版资产一致:`lueying-<version>-setup.exe`。
fn compile_setup(args: &PackArgs, root: &Path) -> Result<PathBuf, String> {
    let iscc = iscc_path()?;
    let script = root.join("installer/lueying.iss");
    let payload = std::path::absolute(&args.out)
        .map_err(|e| format!("解析发布目录 {}: {e}", args.out.display()))?;
    let out_dir = payload
        .parent()
        .ok_or_else(|| "发布目录没有父目录".to_string())?
        .to_path_buf();
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("创建 {}: {e}", out_dir.display()))?;
    let setup = out_dir.join(format!("lueying-{}-setup.exe", args.version));
    let _ = std::fs::remove_file(&setup);

    let mut command = Command::new(&iscc);
    command
        .arg(format!("/DPayloadDir={}", payload.display()))
        .arg(format!("/DOutDir={}", out_dir.display()))
        .arg(format!("/DVersion={}", args.version))
        .arg(format!("/DVersionSafe={}", safe_version(&args.version)))
        .arg(format!("/DVersionInfo={}", version_info(&args.version)));
    command.arg(&script);
    let output = command
        .output()
        .map_err(|e| format!("运行 ISCC({}): {e}", iscc.display()))?;
    if !output.status.success() {
        return Err(format!(
            "ISCC 编译失败:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let bytes = std::fs::metadata(&setup)
        .map_err(|e| format!("读取安装包 {}: {e}", setup.display()))?
        .len();
    println!(
        "安装包 {}: {:.1} MiB",
        setup.display(),
        bytes as f64 / (1024.0 * 1024.0)
    );
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

/// Inno 的 VersionInfoVersion 只接受最多四段数字;预发布后缀不进资源版本。
fn version_info(version: &str) -> String {
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect::<Vec<_>>();
    parts.resize(4, 0);
    format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3])
}

/// 文件名安全的版本号(与安装器 `dll_name` 同一规则)。
fn safe_version(version: &str) -> String {
    version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 复制并记录大小;源文件名与目标名可以不同(DLL 统一成版本化文件名)。
fn copy_file(
    source: &Path,
    out: &Path,
    name: &str,
    files: &mut Vec<(String, u64)>,
) -> Result<(), String> {
    let target = out.join(name);
    let bytes =
        std::fs::copy(source, &target).map_err(|e| format!("复制 {}: {e}", source.display()))?;
    files.push((name.to_string(), bytes));
    Ok(())
}

/// 递归复制目录;清单里记相对路径,安装后能核对每个文件。
fn copy_tree(
    source: &Path,
    target: &Path,
    prefix: &str,
    files: &mut Vec<(String, u64)>,
) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|e| format!("创建 {}: {e}", target.display()))?;
    let entries = std::fs::read_dir(source).map_err(|e| format!("读 {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读 {}: {e}", source.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let relative = format!("{prefix}/{name}");
        if path.is_dir() {
            copy_tree(&path, &target.join(&name), &relative, files)?;
        } else {
            let bytes = std::fs::copy(&path, target.join(&name))
                .map_err(|e| format!("复制 {}: {e}", path.display()))?;
            files.push((relative, bytes));
        }
    }
    Ok(())
}

/// 白霜的词表文件,按质量从高到低。
const FILES: &[&str] = &[
    "base.dict.yaml",
    "8105.dict.yaml",
    "41448.dict.yaml",
    "ext.dict.yaml",
    "tencent.dict.yaml",
];

/// 青简常用词表快照:补入白霜缺失的常用词,并给已收录的多字常用词提频。
/// 单字不参与提频,避免把「的/是/在」这类高频字继续放大成碎切激励。
const COMMON: &str = include_str!("../data/common.tsv");
/// 常用词先验倍数。青简常用词表是独立语料,给多字常用词一个温和的先验;
/// 先按 5 倍验证,句级/难例回归时再回调。
const COMMON_MULT: u64 = 5;
/// 口语异读:同一个词的另一读音,例如「这个」的 `zhei'ge`。白霜只收标准
/// 读音,`zheige` 会被拆成「贼+个」;这里只补 (词,音) 缺失的组合,词频沿用
/// 该词最高频读音,保证两种读法排序一致。
const READINGS: &str = include_str!("../data/readings.tsv");

fn run(args: Args) -> Result<(), String> {
    let mut best: HashMap<(String, String), u64> = HashMap::new();
    let mut unknown: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;
    let mut skipped = 0usize;
    for name in FILES {
        let path = args.frost.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("跳过缺失的 {name}");
            continue;
        };
        let (n, bad) = parse_frost(&text, &mut best, &mut unknown);
        total += n;
        skipped += bad;
        println!("{name}: {n} 行,坏 {bad} 行");
    }
    let (added, boosted, common_bad) = parse_common(COMMON, &mut best);
    println!("common.tsv: 补入 {added} 条,提频 {boosted} 条,坏 {common_bad} 行");
    if common_bad > 0 {
        return Err(format!(
            "common.tsv 有 {common_bad} 行无法解析;补词表坏了会让常用词静默消失"
        ));
    }
    if added + boosted == 0 {
        return Err("common.tsv 没有可用词条;检查 include_str 与文件格式".into());
    }
    let (readings, readings_bad) = parse_readings(READINGS, &mut best);
    println!("readings.tsv: 补入 {readings} 条异读,坏 {readings_bad} 行");
    if readings_bad > 0 {
        return Err(format!(
            "readings.tsv 有 {readings_bad} 行无法解析;异读表坏了会让口语输入静默退化"
        ));
    }
    if readings == 0 {
        return Err("readings.tsv 没有可用异读;检查 include_str 与文件格式".into());
    }
    // 音节表漏一个常用音,词条会被静默地当坏行丢掉(例如 `di` 曾漏掉 2.5 万词条,
    // 用户也打不出该音),所以这里把「未识别音节」显式暴露并在量级异常时中断。
    if !unknown.is_empty() {
        let mut top: Vec<(String, usize)> = unknown.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let list = top
            .iter()
            .take(10)
            .map(|(s, n)| format!("{s}×{n}"))
            .collect::<Vec<_>>()
            .join(",");
        println!("未识别音节: {list}");
        if let Some((s, n)) = top.first().filter(|(_, n)| *n >= 100) {
            return Err(format!(
                "音节表缺少常用音节 {s}({n} 次),这些词条会被静默丢弃;补 crates/dict/src/syllables.rs"
            ));
        }
    }
    let mut lines: Vec<(String, u64, String)> = best
        .into_iter()
        .map(|((word, key), freq)| (word, freq, key))
        .collect();
    lines.sort_by(|a, b| a.2.cmp(&b.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    let mut tsv = String::new();
    for (word, freq, key) in &lines {
        tsv.push_str(&format!("{word}\t{freq}\t{key}\n"));
    }
    std::fs::write(&args.out, &tsv).map_err(|e| format!("写 {}: {e}", args.out.display()))?;
    println!(
        "合计: 收录 {} 条(去重前 {total},坏行 {skipped}) → {}",
        lines.len(),
        args.out.display()
    );
    if let Some(bin) = &args.bin {
        // 与运行期同一条构造路径,保证二进制与 TSV 语义一致。
        let (dict, bad) = lueying_dict::Dictionary::from_text(&tsv);
        if bad != 0 {
            eprintln!("警告: 物化时跳过 {bad} 行");
        }
        let bytes = dict.to_binary();
        std::fs::write(bin, &bytes).map_err(|e| format!("写 {}: {e}", bin.display()))?;
        println!(
            "二进制: {} 条, {} MiB → {}",
            dict.len(),
            bytes.len() / (1024 * 1024),
            bin.display()
        );
    }
    Ok(())
}

/// 解析 Rime dict.yaml:跳过 `...` 之前的头,数据行为 `词<TAB>音节(空格分隔)<TAB>权重`。
fn parse_frost(
    text: &str,
    best: &mut HashMap<(String, String), u64>,
    unknown: &mut HashMap<String, usize>,
) -> (usize, usize) {
    let mut in_data = false;
    let (mut n, mut bad) = (0usize, 0usize);
    for line in text.lines() {
        if !in_data {
            if line.trim() == "..." {
                in_data = true;
            }
            continue;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(word), Some(code), weight) = (parts.next(), parts.next(), parts.next()) else {
            bad += 1;
            continue;
        };
        let sylls: Vec<&str> = code.split_whitespace().collect();
        let weight = weight
            .map(|w| w.trim().trim_end_matches('%'))
            .and_then(|w| w.parse::<u64>().ok())
            .unwrap_or(1);
        if word.chars().count() != sylls.len()
            || !sylls.iter().all(|s| is_syllable(&s.to_lowercase()))
        {
            for s in &sylls {
                let s = s.to_lowercase();
                if !is_syllable(&s) {
                    *unknown.entry(s).or_insert(0) += 1;
                }
            }
            bad += 1;
            continue;
        }
        let key = sylls
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join("'");
        let e = best.entry((word.to_string(), key)).or_insert(0);
        if weight > *e {
            *e = weight;
        }
        n += 1;
    }
    (n, bad)
}

/// `X的` ↔ `X得`:同音异形的孪生词。补词表给前者提频时,后者要同步提,
/// 否则「你说得有道理」会被「你说的有道理」顶掉。
fn de_twin(word: &str) -> Option<String> {
    if let Some(base) = word.strip_suffix('的') {
        Some(format!("{base}得"))
    } else {
        word.strip_suffix('得').map(|base| format!("{base}的"))
    }
}

/// 合并青简常用词表:多字词已收录则提频,未收录则补入;单字整行跳过。
/// 只认仓库内快照,CI 不依赖 work/ 下的参考项目。
fn parse_common(text: &str, best: &mut HashMap<(String, String), u64>) -> (usize, usize, usize) {
    let existing: HashSet<String> = best.keys().map(|(word, _)| word.clone()).collect();
    let mut entries: Vec<(String, String, u64)> = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(word), Some(freq), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
            bad += 1;
            continue;
        };
        let chars = word.chars().count();
        // 单字不提频,避免把「的/是/在」这类高频字继续放大成碎切激励。
        if chars < 2 || !word.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            continue;
        }
        let Ok(freq) = freq.parse::<u64>() else {
            bad += 1;
            continue;
        };
        let sylls: Vec<&str> = key.split('\'').collect();
        if sylls.len() != chars || !sylls.iter().all(|s| is_syllable(s)) {
            bad += 1;
            continue;
        }
        entries.push((word.to_string(), key.to_string(), freq));
    }
    let mut common_freq: HashMap<&str, u64> = HashMap::new();
    for (word, _, freq) in &entries {
        let slot = common_freq.entry(word.as_str()).or_insert(0);
        *slot = (*slot).max(*freq);
    }
    // 「X的」提频时,同音键的「X得」不提会把 `nishuodeyoudaoli` 推向
    // 「你说的有道理」。两者只靠语法模型分,词频比例保持白霜原样。
    let mut siblings: HashSet<String> = HashSet::new();
    for (word, key) in best.keys() {
        if !common_freq.contains_key(word.as_str()) {
            continue;
        }
        let Some(twin) = de_twin(word) else {
            continue;
        };
        if best.contains_key(&(twin.clone(), key.clone())) {
            siblings.insert(twin);
        }
    }
    let mut boosted = 0usize;
    for ((word, _), freq) in best.iter_mut() {
        if word.chars().count() < 2 {
            continue;
        }
        if let Some(&base) = common_freq.get(word.as_str()) {
            // 白霜里 0 频等同禁用;常用词表给了正频就把词启用,否则照常提频。
            *freq = if *freq == 0 {
                base
            } else {
                freq.saturating_mul(COMMON_MULT)
            };
            boosted += 1;
        } else if siblings.contains(word.as_str()) {
            *freq = freq.saturating_mul(COMMON_MULT);
            boosted += 1;
        }
    }
    drop(common_freq);
    drop(siblings);
    // 白霜已有这个词时只提频,不把青简的另一种读音追加进来;缺词才补一个最高频读音。
    let mut added = 0usize;
    for (word, key, freq) in entries {
        if !existing.contains(&word) {
            best.insert((word, key), freq);
            added += 1;
        }
    }
    (added, boosted, bad)
}

/// 合并口语异读表。格式 `词<TAB>音节`,两列;词频从该词已有的读音继承,
/// 因此异读与主读音同权。词或音节表里没有的词直接算坏行:异读表是手写的,
/// 静默跳过会让「zheige 打不出这个」这类退化难以发现。
fn parse_readings(text: &str, best: &mut HashMap<(String, String), u64>) -> (usize, usize) {
    let mut added = 0usize;
    let mut bad = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(word), Some(key)) = (parts.next(), parts.next()) else {
            bad += 1;
            continue;
        };
        let sylls: Vec<&str> = key.split('\'').collect();
        if !word.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            || sylls.len() != word.chars().count()
            || !sylls.iter().all(|s| is_syllable(s))
        {
            bad += 1;
            continue;
        }
        let ident = (word.to_string(), key.to_string());
        if best.contains_key(&ident) {
            continue;
        }
        let Some(freq) = best
            .iter()
            .filter(|((w, _), _)| w.as_str() == word)
            .map(|(_, freq)| *freq)
            .max()
        else {
            bad += 1;
            continue;
        };
        best.insert(ident, freq);
        added += 1;
    }
    (added, bad)
}
