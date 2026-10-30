//! 自定义短语扩展。
//!
//! 文件格式与常见输入法一致:`短语<TAB>编码[<TAB>权重]`。编码写全拼
//! (`ni'hao`)或简拼(`nh`);带隔音符时简拼按各音节首字母生成。短语固定
//! 上屏,不写用户词库,也不参与整句造词。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};

/// 首次启动写出的短语文件模板。示例全部注释,不改变默认候选。
pub const TEMPLATE: &str = "\
# 掠影自定义短语
# 格式:短语<TAB>编码[<TAB>权重]
# 编码可写全拼(ni'hao)或简拼(nh);不带隔音符时按完整编码匹配。
# 在设置窗「自定义短语 → 编辑」里改并保存,或改完文件重启输入法即可生效。
#
# 示例:
# 你好\tni'hao\t100
# 我的邮箱\two'de'you'xiang\t50
";

#[derive(Clone)]
struct Phrase {
    text: String,
    /// 去掉隔音符的小写编码。
    code: String,
    /// 编码各音节首字母;编码没有隔音符时为空。
    initials: String,
    weight: u32,
}

/// 短语与编码索引。编码与简拼共用一张表:每个短语在各自的键下登记一次,
/// 查询只看命中的那几条,短语文件很大时也不会每敲一键扫全表。
struct Store {
    phrases: Vec<Phrase>,
    index: HashMap<String, Vec<usize>>,
}

impl Store {
    fn new(phrases: Vec<Phrase>) -> Self {
        let mut index: HashMap<String, Vec<usize>> = HashMap::new();
        for (at, phrase) in phrases.iter().enumerate() {
            for key in [&phrase.code, &phrase.initials] {
                if key.is_empty() {
                    continue;
                }
                let slots = index.entry(key.clone()).or_default();
                if slots.last() != Some(&at) {
                    slots.push(at);
                }
            }
        }
        Self { phrases, index }
    }

    fn len(&self) -> usize {
        self.phrases.len()
    }

    fn matches(&self, query: &str) -> Vec<&Phrase> {
        let Some(slots) = self.index.get(query) else {
            return Vec::new();
        };
        slots.iter().map(|&at| &self.phrases[at]).collect()
    }
}

/// 自定义短语模块。文件在首次使用时读取,保存后可由宿主调用 [`Self::reload`]。
pub struct CustomPhrases {
    path: Option<PathBuf>,
    phrases: RwLock<Store>,
}

impl CustomPhrases {
    /// 空模块;测试或未配置文件时使用。
    pub fn empty() -> Self {
        Self {
            path: None,
            phrases: RwLock::new(Store::new(Vec::new())),
        }
    }

    /// 从文本构造,便于测试与导入。
    pub fn from_text(text: &str) -> Self {
        Self {
            path: None,
            phrases: RwLock::new(Store::new(parse(text))),
        }
    }

    /// 从文件构造;文件缺失或读取失败时为空模块,不阻断输入法启动。
    pub fn load(path: PathBuf) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        Self {
            path: Some(path),
            phrases: RwLock::new(Store::new(parse(&text))),
        }
    }

    /// 重新读取文件。设置页保存短语后调用;读失败时保留上一份内容。
    pub fn reload(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        *self.phrases.write().unwrap_or_else(|p| p.into_inner()) = Store::new(parse(&text));
    }

    /// 当前短语条数。
    pub fn len(&self) -> usize {
        self.phrases.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl Module for CustomPhrases {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "custom-phrases",
            name: "自定义短语",
            version: env!("CARGO_PKG_VERSION"),
            capabilities: &[Capability::Candidates],
        }
    }

    fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
        // 辅码正在筛拼音候选;短语没有字形依据,不掺进去。
        if ctx.raw.contains('`') {
            return;
        }
        let query = normalize(ctx.query);
        if query.is_empty() {
            return;
        }
        let phrases = self.phrases.read().unwrap_or_else(|p| p.into_inner());
        let mut matched = phrases.matches(&query);
        if matched.is_empty() {
            return;
        }
        matched.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.text.cmp(&b.text)));
        for (index, phrase) in matched.into_iter().enumerate() {
            // 短语优先:同文的词典候选让位,避免首页出现两个完全相同的词。
            cands.retain(|cand| cand.text != phrase.text);
            let mut cand =
                Candidate::whole(ctx.raw.len(), phrase.text.clone(), 1.0e9 - index as f64);
            cand.quick = true;
            cand.hint = Some("自定义短语".to_string());
            let at = index.min(cands.len());
            cands.insert(at, cand);
        }
    }
}

fn parse(text: &str) -> Vec<Phrase> {
    // 记事本另存为 UTF-8 时可能带 BOM;否则首行会被当成坏行。
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let mut parts = line.split('\t');
        let Some(text) = parts.next().map(str::trim).filter(|text| !text.is_empty()) else {
            continue;
        };
        let Some(code) = parts.next().map(str::trim).filter(|code| !code.is_empty()) else {
            continue;
        };
        let weight = parts
            .next()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(1)
            .max(1);
        let Some((code, initials)) = normalize_code(code) else {
            continue;
        };
        out.push(Phrase {
            text: text.to_string(),
            code,
            initials,
            weight,
        });
    }
    out
}

/// 规范编码:小写、只留字母与隔音符;同时给出各音节首字母。
fn normalize_code(code: &str) -> Option<(String, String)> {
    let mut plain = String::new();
    let mut initials = String::new();
    let mut syllables = 0usize;
    for part in code.to_ascii_lowercase().split('\'') {
        let part: String = part.chars().filter(char::is_ascii_alphabetic).collect();
        if part.is_empty() {
            continue;
        }
        syllables += 1;
        if let Some(first) = part.chars().next() {
            initials.push(first);
        }
        plain.push_str(&part);
    }
    if plain.is_empty() {
        return None;
    }
    // 没有隔音符时无法可靠切音节,简拼只保留完整编码匹配。
    if syllables < 2 {
        initials.clear();
    }
    Some((plain, initials))
}

fn normalize(query: &str) -> String {
    query
        .chars()
        .filter(char::is_ascii_alphabetic)
        .flat_map(char::to_lowercase)
        .collect()
}
