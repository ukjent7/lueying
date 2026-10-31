//! 发布打包:装配可执行文件与数据、生成清单与版本号、编 Inno 安装包。
//!
//! `main` 的 `#[path]` 子模块(src 保持扁平)。

use std::path::{Path, PathBuf};
use std::process::Command;


pub(crate) struct PackArgs {
    bin: PathBuf,
    /// i686 构建目录;提供时打包 32 位 TSF DLL。
    bin32: Option<PathBuf>,
    data: PathBuf,
    out: PathBuf,
    version: String,
    allow_missing_data: bool,
}

pub(crate) fn parse_pack(it: &mut impl Iterator<Item = String>) -> PackArgs {
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

pub(crate) fn pack(args: PackArgs) -> Result<(), String> {
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
