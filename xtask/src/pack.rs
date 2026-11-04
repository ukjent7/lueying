//! 发布打包:装配可执行文件与数据、生成清单与版本号、编 Inno 安装包。
//!
//! `main` 的 `#[path]` 子模块(src 保持扁平)。

use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) struct PackArgs {
    bin: PathBuf,
    data: PathBuf,
    out: PathBuf,
    version: String,
    allow_missing_data: bool,
}

pub(crate) fn parse_pack(it: &mut impl Iterator<Item = String>) -> PackArgs {
    let mut args = PackArgs {
        bin: PathBuf::from("target/release"),
        data: PathBuf::from("data"),
        out: PathBuf::from("dist/lueying"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        allow_missing_data: false,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bin" => args.bin = it.next().map(PathBuf::from).unwrap_or(args.bin),
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
    "lueying-broker.exe",
    "lueying-settings.exe",
    "lueying-user.exe",
];
const PACK_DLLS: &[&str] = &["lueying_tsf.dll", "lueying-tsf.dll"];
/// 默认包带精确词典、8M/25M 按键重排模型与原生紧凑搭配表。`collocation.bin` 缺失时
/// 引擎退回纯词频;发布包由 `tools/fetch_collocation.sh` 钉版本。
/// PP-OCR 模型体积大,仍在设置窗「官方扩展」里按需下载,详见 `Catalog/official-extensions.json`。
const PACK_DATA: &[&str] = &[
    "main.pyd",
    "collocation.bin",
    "rerank-8m.ort",
    "rerank-8m.model.json",
    "rerank-8m.vocab.json",
    "rerank-25m.ort",
    "rerank-25m.model.json",
    "rerank-25m.vocab.json",
];
/// 发布体积门禁:25M 随包后发布目录约 183 MiB、安装包约 95 MiB。
/// 上限留约 20% 余量,新增模型或依赖超过这里要显式改门限并评估下载/安装成本。
const MAX_PAYLOAD_BYTES: u64 = 220 * 1024 * 1024;
const MAX_SETUP_BYTES: u64 = 120 * 1024 * 1024;

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
    for name in PACK_DATA {
        // fetch_rerank.sh 默认下载到 data/rerank/,发布目录却是平铺的;两种布局都接受。
        let flat = args.data.join(name);
        let source = if flat.exists() {
            flat
        } else if *name == "collocation.bin" && args.data.join("main.gram").exists() {
            args.data.join("main.gram")
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
    // main.gram 别名兼容
    if args.data.join("main.gram").exists() && !args.out.join("main.gram").exists() {
        let _ = copy_file(
            &args.data.join("main.gram"),
            &args.out,
            "main.gram",
            &mut files,
        );
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
    // 模型仓库的 NOTICE 是训练语料署名,Apache-2.0 要求随权重一起再分发。
    let model_notice = [args.data.join("rerank/NOTICE"), args.data.join("NOTICE")]
        .into_iter()
        .find(|path| path.exists());
    if let Some(path) = model_notice {
        copy_file(&path, &licenses, "NOTICE", &mut files)?;
    }
    let notice = "\
# 随包第三方许可与署名

- 白霜拼音词库:GPL-3.0,https://github.com/gaboolic/rime-frost,\
commit fbdf9efc4a5036db5f37b97323e1b936e8fe993c。
- 青简常用词表与词频:GPL-3.0-or-later,https://github.com/qingjian-team/qingjian,\
版本 0.1.1;随包词典用这份常用词表补入白霜未收录的词,并给多字常用词提频。\
词频语料来源包括中文维基百科(CC BY-SA 4.0)、LCCC(MIT)与 THUOCL(MIT),\
详见青简词库头部说明。
- 掠影原生紧凑搭配表 (LYCL v2):GPL-3.0-or-later,https://github.com/ukjent7/lueying-dict,\
版本 v0.2.0-rc0 (collocation-v2.bin);基于点互信息 (PMI) 与 Ultra-FineWeb、LCCC 语料统计,\
由掠影输入法工具链原生切分生成 (4.77MiB, 999,863 条词对);\
sha256 f7a42f8a5de4d3e932c569bc292cee30994af6ab04cd2f087f93a2498ba171d6。\
许可全文见随包的 LICENSE。
- chinese-ime-lm:Apache-2.0,https://github.com/metasequoiaime/chinese-ime-lm,\
commit 1c90f5ee1b76ecf18d5171ef78a4dc076a0bb0d8 (release rerank-v2);全文见 Apache-2.0.txt。
- pinyin-ime-keystroke-reranker-8M:Apache-2.0,\
https://huggingface.co/msime/pinyin-ime-keystroke-reranker-8M,\
revision ea91fcbc94ab8d801faab250ee02d78ed0de5b77。
- pinyin-ime-keystroke-reranker-25M:Apache-2.0,\
https://huggingface.co/msime/pinyin-ime-keystroke-reranker-25M,\
revision 9ce963999bd40ebede9f73f7e39a200e8d50f25e。
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

    // 官方起步扩展随安装包分发(Wasm guest + 清单文本);设置窗按需复制到当前用户目录。
    // 只收带清单的目录:`extensions/input-schemes` 是编译期方案扩展的源码,不进安装包。
    let extensions = root.join("extensions");
    if extensions.is_dir() {
        let target = args.out.join("extensions");
        std::fs::create_dir_all(&target).map_err(|e| format!("创建 {}: {e}", target.display()))?;
        let entries = std::fs::read_dir(&extensions)
            .map_err(|e| format!("读 {}: {e}", extensions.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("读 {}: {e}", extensions.display()))?;
            let path = entry.path();
            if !path.is_dir() || !path.join("manifest.json").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            copy_tree(
                &path,
                &target.join(&name),
                &format!("extensions/{name}"),
                &mut files,
            )?;
        }
    }
    // 官方扩展目录也放一份在发布目录里,供审计与离线检查;程序运行时用
    // 编译期嵌入的同一份文件,不依赖这个副本。
    let catalog = root.join("Catalog");
    if catalog.is_dir() {
        copy_tree(&catalog, &args.out.join("Catalog"), "Catalog", &mut files)?;
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
    if total > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "发布目录 {:.1} MiB 超过体积门禁 {:.1} MiB",
            total as f64 / (1024.0 * 1024.0),
            MAX_PAYLOAD_BYTES as f64 / (1024.0 * 1024.0)
        ));
    }
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
    if bytes > MAX_SETUP_BYTES {
        return Err(format!(
            "安装包 {}: {:.1} MiB 超过体积门禁 {:.1} MiB",
            setup.display(),
            bytes as f64 / (1024.0 * 1024.0),
            MAX_SETUP_BYTES as f64 / (1024.0 * 1024.0)
        ));
    }
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
