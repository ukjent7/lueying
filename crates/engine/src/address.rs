//! 邮箱与网址:中文态打 `用户名@` 出常见邮箱完整地址,`www.` 开头的网址
//! 原样上屏。地址不是用户词:不学习、不造词、不参与模型重排,与快捷候选
//! 同一语义。

use crate::query::Candidate;

/// 常见邮箱域;按国内常用度排前,候选一页能看全。
const DOMAINS: [&str; 15] = [
    "qq.com",
    "163.com",
    "126.com",
    "foxmail.com",
    "gmail.com",
    "outlook.com",
    "hotmail.com",
    "icloud.com",
    "sina.com",
    "sohu.com",
    "yeah.net",
    "aliyun.com",
    "msn.com",
    "live.com",
    "139.com",
];

/// 用户名字符:小写字母与数字。带点的用户名(如 `john.doe`)打不出来——
/// @ 之前的点会先按句号上屏,这是中文态的正常语义;纯数字开头的 QQ 号
/// 邮箱同样进不来,数字键在无组合时直接上屏。这类地址请切英文模式。
pub(crate) fn is_local(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'0'..=b'9')
}

/// 域名字符:小写字母、数字与 `.`。
pub(crate) fn is_domain(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.')
}

/// 用户名后接 @ 的时机:已有非空用户名,组合里还没有 @。
pub(crate) fn email_at_ok(raw: &str) -> bool {
    !raw.is_empty() && raw.bytes().all(is_local)
}

/// 组合是否处于邮箱形态:用户名合法,@ 后只收字母、数字与点。
pub(crate) fn in_email(raw: &str) -> bool {
    let Some((local, domain)) = raw.split_once('@') else {
        return false;
    };
    !local.is_empty() && local.bytes().all(is_local) && domain.bytes().all(is_domain)
}

/// 组合是否处于网址形态:`www.` 开头。其余域名不接管,避免吞掉
/// 「好。」这类正常句号。
pub(crate) fn in_url(raw: &str) -> bool {
    raw.starts_with("www.")
}

/// 邮箱/网址形态下点号并入组合的时机:网址进行中,或 @ 后域名已开始
/// 且未以点结尾。`www` 刚打完时也并,让 `www.` 能起头。
pub(crate) fn dot_ok(raw: &str) -> bool {
    if raw == "www" || (in_url(raw) && !raw.ends_with('.')) {
        return true;
    }
    let Some((_, domain)) = raw.split_once('@') else {
        return false;
    };
    !domain.is_empty() && !domain.ends_with('.') && domain.bytes().all(is_domain)
}

/// 当前原始串的地址候选:邮箱优先,其次网址直通。
pub(crate) fn candidates(raw: &str) -> Vec<Candidate> {
    if in_email(raw) {
        return email_candidates(raw);
    }
    url_candidates(raw)
}

/// 邮箱候选:常见域按 @ 后已输入的前缀筛,末尾补原文以容纳表外域名。
fn email_candidates(raw: &str) -> Vec<Candidate> {
    let Some((local, typed)) = raw.split_once('@') else {
        return Vec::new();
    };
    let mut out: Vec<Candidate> = DOMAINS
        .iter()
        .filter(|domain| domain.starts_with(typed))
        .take(9)
        .enumerate()
        .map(|(index, domain)| {
            let mut cand =
                Candidate::quick(raw.len(), format!("{local}@{domain}"), 1.0e9 - index as f64);
            cand.hint = Some("邮箱".to_string());
            cand
        })
        .collect();
    if !out.iter().any(|cand| cand.text == raw) {
        let mut cand = Candidate::quick(raw.len(), raw.to_string(), 0.0);
        cand.hint = Some("原文".to_string());
        out.push(cand);
    }
    out
}

/// 网址直通候选:原样上屏,继续输入时跟着更新。
fn url_candidates(raw: &str) -> Vec<Candidate> {
    if !in_url(raw) {
        return Vec::new();
    }
    let mut cand = Candidate::quick(raw.len(), raw.to_string(), 0.0);
    cand.hint = Some("网址".to_string());
    vec![cand]
}
