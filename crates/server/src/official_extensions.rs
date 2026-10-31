//! 官方扩展目录:随程序分发的 `Catalog/official-extensions.json` 是唯一来源。
//!
//! 目录只描述"能装什么"与"从哪装",不信任远端清单:每个下载文件都带
//! SHA-256 与字节数,安装器逐项校验后才落盘。扩展包本身是文本 `.luext`
//! 或随包的本地目录;模型走 `%APPDATA%\lueying\models`。

use std::sync::LazyLock;

use serde::Deserialize;

/// 目录文件在编译期嵌入,发布目录里也放一份供审计。
pub const CATALOG_JSON: &str = include_str!("../../../Catalog/official-extensions.json");

/// 目录里的一个可安装项。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OfficialExtension {
    /// 稳定 id,也是扩展目录名或模型标识。
    pub id: String,
    pub name: String,
    pub version: String,
    /// `extension` 或 `model`。
    pub kind: String,
    pub description: String,
    /// 面向用户的体积说明。
    pub size: String,
    /// 扩展的随包来源(相对安装目录);只对 `extension` 有效。
    #[serde(default)]
    pub bundled: Option<String>,
    /// 需要下载的文件;模型与远程扩展都用它。
    #[serde(default)]
    pub files: Vec<OfficialFile>,
}

/// 目录里的一个下载文件。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OfficialFile {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

/// 目录根。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Catalog {
    pub version: u32,
    pub extensions: Vec<OfficialExtension>,
}

/// 解析后的官方目录;坏文件是构建错误,这里 `expect` 让它尽早暴露。
pub fn catalog() -> &'static Catalog {
    static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
        serde_json::from_str(CATALOG_JSON).expect("Catalog/official-extensions.json 必须是合法目录")
    });
    &CATALOG
}

/// 按 id 找目录项。
pub fn find(id: &str) -> Option<&'static OfficialExtension> {
    catalog()
        .extensions
        .iter()
        .find(|extension| extension.id == id)
}

impl OfficialExtension {
    /// 是否扩展包。
    pub fn is_extension(&self) -> bool {
        self.kind == "extension"
    }

    /// 是否模型包。
    pub fn is_model(&self) -> bool {
        self.kind == "model"
    }

    /// 模型是否已经可用。
    pub fn installed_model(&self) -> bool {
        match self.id.as_str() {
            "ppocr" => crate::config::ppocr_dir().is_some(),
            "rerank-25m" => crate::config::model_path("rerank-25m.safetensors").is_some(),
            _ => false,
        }
    }
}

/// PP-OCR 词表:模型下载含权重,词表随程序分发,避免再依赖一个可变的 raw URL。
pub const PPOCR_DICT: &str = include_str!("../../../Catalog/ppocr-dict.txt");

#[cfg(test)]
#[path = "official_extensions_tests.rs"]
mod tests;
