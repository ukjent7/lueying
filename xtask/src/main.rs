//! 仓库任务:
//! - `dict`:把白霜拼音(Rime dict.yaml)转成本项目的 TSV/二进制词典。
//! - `pack`:把可执行文件与数据装进发布目录,生成清单与版本号。
//!
//! 用法:`cargo run -p xtask -- dict --frost <白霜cn_dicts目录> --out <输出.tsv>`
//! 用法:`cargo run -p xtask -- pack --bin target/release --bin32 target/i686-pc-windows-msvc/release --data data --out dist/lueying`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
}

fn main() {
    let mut task = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "dict" => task = Some(Task::Dict(parse_dict(&mut it))),
            "pack" => task = Some(Task::Pack(parse_pack(&mut it))),
            other => {
                eprintln!("未知子命令或参数: {other}");
                std::process::exit(2);
            }
        }
    }
    let result = match task {
        Some(Task::Dict(args)) => run(args),
        Some(Task::Pack(args)) => pack(args),
        None => {
            eprintln!("用法: xtask dict ... | xtask pack ...");
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

/// 发布目录的必需文件。DLL 文件名随 Cargo 包名生成,两种拼写都接受。
const PACK_BINS: &[&str] = &[
    "lueying-server.exe",
    "lueying-settings.exe",
    "lueying-install.exe",
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
    // 版本号入 DLL 文件名:升级时新文件不被正在运行的旧 DLL 锁住。
    let dll_name = format!("lueying_tsf-{}.dll", safe_version(&args.version));
    copy_file(&dll, &args.out, &dll_name, &mut files)?;
    if let Some(bin32) = &args.bin32 {
        let dll32 = PACK_DLLS
            .iter()
            .map(|name| bin32.join(name))
            .find(|path| path.exists())
            .ok_or_else(|| format!("{} 里缺少 32 位 lueying_tsf.dll", bin32.display()))?;
        let dll32_name = format!("lueying_tsf32-{}.dll", safe_version(&args.version));
        copy_file(&dll32, &args.out, &dll32_name, &mut files)?;
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
";
    std::fs::write(licenses.join("README.md"), notice)
        .map_err(|e| format!("写 licenses/README.md: {e}"))?;
    files.push(("licenses/README.md".into(), notice.len() as u64));

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
    Ok(())
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

/// 白霜的词表文件,按质量从高到低。
const FILES: &[&str] = &[
    "base.dict.yaml",
    "8105.dict.yaml",
    "41448.dict.yaml",
    "ext.dict.yaml",
    "tencent.dict.yaml",
];

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
