//! 重排模型按需下载:安装包只带词典与搭配表,两档神经重排模型(约 38 MiB)
//! 落到用户数据目录,升级与卸载都不动它,只下一次。
//!
//! 模型缺失时引擎只保留词频候选(见服务端 `load_model`),输入不受影响,
//! 所以断网装完也能打字,模型随后补齐。钉版与 `tools/fetch_rerank.sh` 一致:
//! 同一个上游 Release 标签、同一组 sha256,校验不过不留文件。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lueying_server::config;

use crate::net;
use crate::update::sha256_file;

/// 上游钉版 Release;与 `tools/fetch_rerank.sh` 的 `RERANK_TAG` 默认值一致。
const BASE: &str = "https://github.com/ukjent7/chinese-ime-lm/releases/download/rerank-v2";
/// 单文件上限:最大的 25M 权重约 30 MiB,留一倍余量防失控响应。
const ASSET_LIMIT: u64 = 64 * 1024 * 1024;
/// 大文件按慢网给足接收时间;整体时限而不是单次读超时。
const TIMEOUT: Duration = Duration::from_secs(600);

struct Asset {
    name: &'static str,
    sha256: &'static str,
}

/// 两档模型各三个文件必须同目录;LICENSE/NOTICE 是 Apache-2.0 要求随权重
/// 一起再分发的署名,一并落到模型目录。
const ASSETS: &[Asset] = &[
    Asset {
        name: "rerank-8m.ort",
        sha256: "19c04188e2ee102bd264925e3ba0a329960de9b70eefcfc87ad95161a665e96c",
    },
    Asset {
        name: "rerank-8m.model.json",
        sha256: "33e2b903a0d576730488864a007e67d518d8c7123572e6f4f77a80487fb5eda5",
    },
    Asset {
        name: "rerank-8m.vocab.json",
        sha256: "d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038",
    },
    Asset {
        name: "rerank-25m.ort",
        sha256: "e2d97b943aa727d01f6efa9c48dde1ffcac079a2b5168564ffbdc5ab7146d922",
    },
    Asset {
        name: "rerank-25m.model.json",
        sha256: "fdeb4f150fd6a9f937b130ccef4f3248436a9b783202e125fd0a2d1fe3661283",
    },
    Asset {
        name: "rerank-25m.vocab.json",
        sha256: "d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038",
    },
    Asset {
        name: "LICENSE",
        sha256: "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
    },
    Asset {
        name: "NOTICE",
        sha256: "e120d424d4d7d8cc006c7adcd02a748e83a07cdae8f35f31fefd431e56168d1e",
    },
];

/// 后台任务把本地已验证的权重预打包成自包含 safetensors;ORT 原文件保留不删。
#[cfg(windows)]
const PACKED_MODELS: [(&str, &str); 2] = [
    (
        "rerank-8m",
        "896b21eefbf94118a99fa84d8469a28a60e3728869fbe0e705a4d96b82eb158e",
    ),
    (
        "rerank-25m",
        "bccf14f5b4ac16176db9d18111c15aa87e542d5648348c43f2a578af0c95e1fb",
    ),
];

/// 模型目录;与服务端 `load_model` 的首查目录同一个。
pub fn dir() -> Option<PathBuf> {
    config::models_dir()
}

/// 已就绪(存在且哈希对)的文件数与总数。
pub fn ready() -> (usize, usize) {
    let Some(dir) = dir() else {
        return (0, ASSETS.len());
    };
    let done = ASSETS
        .iter()
        .filter(|asset| matches(&dir.join(asset.name), asset.sha256))
        .count();
    (done, ASSETS.len())
}

/// 补齐缺失或哈希不符的文件;全部就绪时一个请求都不发。返回本次补齐的文件数。
/// 旧版本把模型装在程序目录,那里有有效副本就本地搬一份,不重新下载。
pub fn ensure() -> Result<usize, String> {
    let dir = dir().ok_or("定位不到模型目录")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {}: {e}", dir.display()))?;
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let mut placed = 0;
    for asset in ASSETS {
        let target = dir.join(asset.name);
        if matches(&target, asset.sha256) {
            continue;
        }
        if let Some(source) = beside.as_ref().map(|dir| dir.join(asset.name))
            && matches(&source, asset.sha256)
            && std::fs::copy(&source, &target).is_ok()
        {
            placed += 1;
            continue;
        }
        fetch(asset, &target)?;
        placed += 1;
    }
    #[cfg(windows)]
    for (stem, digest) in PACKED_MODELS {
        if let Err(error) = ensure_packed(&dir, stem, digest) {
            eprintln!("重排模型 {stem} 预打包失败，保留原模型: {error}");
        }
    }
    Ok(placed)
}

/// 把同目录已验证的 ORT 三件套编译成 `{stem}.packed.safetensors`;只打包不下载,
/// 写到 `.part` 校验后再就位,失败不留半截文件也不动原权重。
#[cfg(windows)]
fn ensure_packed(dir: &Path, stem: &str, digest: &str) -> Result<(), String> {
    let target = dir.join(format!("{stem}.packed.safetensors"));
    if matches(&target, digest) {
        return Ok(());
    }
    let part = target.with_extension(format!("safetensors.{}.part", std::process::id()));
    let mut file = std::fs::File::create_new(&part)
        .map_err(|error| format!("创建 {}: {error}", part.display()))?;
    let packed = lueying_rerank::Model::pack_keystroke_dir(dir, stem, &mut file)
        .and_then(|()| file.flush().map_err(|error| error.to_string()));
    drop(file);
    let result = packed.and_then(|()| {
        if !matches(&part, digest) {
            return Err("SHA-256 校验失败".into());
        }
        match std::fs::rename(&part, &target) {
            Ok(()) => Ok(()),
            Err(_) if matches(&target, digest) => Ok(()),
            Err(error) => Err(format!("就位 {}: {error}", target.display())),
        }
    });
    let _ = std::fs::remove_file(&part);
    result
}

/// 哈希对得上才算就绪;算不动(文件损坏/占用)按未就绪处理,交给下载覆盖。
fn matches(path: &Path, expected: &str) -> bool {
    sha256_file(path).is_ok_and(|sha| sha.eq_ignore_ascii_case(expected))
}

/// 下到 `.part` 再校验改名:失败或校验不过都不留看着齐全的半截文件。
fn fetch(asset: &Asset, target: &Path) -> Result<(), String> {
    let url = format!("{BASE}/{}", asset.name);
    // 安装器与设置窗可能同时补齐;临时名带进程号,互不覆盖。
    let part = target.with_file_name(format!("{}.{}.part", asset.name, std::process::id()));
    let result = (|| -> Result<(), String> {
        let agent = net::agent(&url, TIMEOUT)?;
        let response = agent
            .get(&url)
            .call()
            .map_err(|e| format!("请求 {url} 失败: {e}"))?;
        let mut file =
            std::fs::File::create(&part).map_err(|e| format!("创建 {}: {e}", part.display()))?;
        let mut reader = response
            .into_body()
            .into_with_config()
            .limit(ASSET_LIMIT)
            .reader();
        std::io::copy(&mut reader, &mut file).map_err(|e| format!("下载 {}: {e}", asset.name))?;
        file.flush()
            .map_err(|e| format!("写 {}: {e}", part.display()))?;
        drop(file);
        if !matches(&part, asset.sha256) {
            return Err(format!("{} SHA-256 校验失败", asset.name));
        }
        // 目标可能被服务端映射着;改名失败就留 .part,下次启动重试。
        std::fs::rename(&part, target).map_err(|e| format!("就位 {}: {e}", asset.name))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}
