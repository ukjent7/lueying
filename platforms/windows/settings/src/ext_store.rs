#[cfg(any(windows, test))]
use lueying_ext::manifest::container;
use lueying_ext::manifest::{Manifest, consent};

#[derive(Clone, Copy)]
pub struct Official {
    pub id: &'static str,
    pub name: &'static str,
    pub detail: &'static str,
    pub shortcut: &'static str,
    pub command: &'static str,
    pub version: &'static str,
    manifest_json: &'static [u8],
}

pub const OFFICIAL: &[Official] = &[
    Official {
        id: "shot-ocr",
        name: "截图识别",
        detail: "框选屏幕上的文字，识别后即可使用。",
        shortcut: "Ctrl + Alt + O",
        command: "shot-ocr.run",
        version: "0.1.0",
        manifest_json: include_bytes!("../../../../extensions/shot-ocr/manifest.json"),
    },
    Official {
        id: "voice-asr",
        name: "离线语音输入",
        detail: "按下快捷键录音，识别结果直接上屏。需另备本地语音识别引擎。",
        shortcut: "Ctrl + Alt + V",
        command: "voice-asr.dictate",
        version: "0.1.0",
        manifest_json: include_bytes!("../../../../extensions/voice-asr/manifest.json"),
    },
    Official {
        id: "clipboard-history",
        name: "剪贴板历史",
        detail: "从最近复制的内容中选择一条粘贴。",
        shortcut: "Win + Shift + V",
        command: "clipboard-history.show",
        version: "0.1.0",
        manifest_json: include_bytes!("../../../../extensions/clipboard-history/manifest.json"),
    },
];

pub fn find(id: &str) -> Option<Official> {
    OFFICIAL.iter().copied().find(|item| item.id == id)
}

impl Official {
    /// 随设置窗编入的清单所请求的能力。只供下载前预览，不记录同意。
    pub fn permission_preview(self) -> Result<Vec<String>, String> {
        let manifest = self.parse_manifest(self.manifest_json)?;
        Ok(consent::consent_items(&manifest))
    }

    fn parse_manifest(self, raw: &[u8]) -> Result<Manifest, String> {
        let manifest = Manifest::parse(raw)?;
        if manifest.id != self.id || manifest.version != self.version || manifest.is_native() {
            return Err("扩展清单与商店条目不符".into());
        }
        Ok(manifest)
    }
}

#[cfg(any(windows, test))]
impl Official {
    fn url(self) -> String {
        format!(
            "https://github.com/ukjent7/lueying/releases/download/{0}-v{1}/{0}.luext",
            self.id, self.version
        )
    }

    /// 核对包内的清单声明，不验证负载的签名。
    fn verify(self, bytes: &[u8]) -> Result<container::Files, String> {
        let files = container::decode(bytes)?;
        let (_, raw) = files
            .iter()
            .find(|(name, _)| name == "manifest.json")
            .ok_or("下载的扩展没有清单")?;
        let manifest = self.parse_manifest(raw)?;
        let expected = self.parse_manifest(self.manifest_json)?;
        // 同意文案会合并重叠的能力与服务，核对清单声明本身，而不是文案。
        if manifest.capabilities != expected.capabilities
            || manifest.needs != expected.needs
            || manifest.network != expected.network
        {
            return Err("下载包的权限声明与商店预览不符，未安装".into());
        }
        Ok(files)
    }

    #[cfg(windows)]
    pub(crate) fn download(self) -> Result<(), String> {
        let root = lueying_server::config::extensions_dir().ok_or("找不到扩展目录")?;
        let bytes = crate::ext_add::download(&self.url(), 16 << 20)
            .map_err(|e| format!("无法下载官方扩展，请检查网络或稍后重试: {e}"))?;
        let files = self.verify(&bytes)?;
        crate::ext_install::install_new_files(&files, &root)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "ext_store_tests.rs"]
mod tests;
