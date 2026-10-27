//! 快捷输入:日期、时间、星期、v 模式符号/数字/金额/日期/算式与 u 问字。
//!
//! 这些候选只回答当前原始串,不是词典词:上屏时不写用户词库、不学词级转移,
//! 也不会被第二天的同一触发串召回成旧日期。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::query::Candidate;
use crate::symbols;

const LOWER_DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
/// 逐位读法专用:阿拉伯数字的 0 读「〇」,位值读法的空缺才用「零」。
const CHAR_DIGITS: [&str; 10] = ["〇", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
const UPPER_DIGITS: [&str; 10] = ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"];
const LOWER_UNITS: [&str; 4] = ["", "十", "百", "千"];
const UPPER_UNITS: [&str; 4] = ["", "拾", "佰", "仟"];
const GROUP_UNITS: [&str; 2] = ["", "万"];
/// 最多 16 位整数,覆盖到「千万亿」;更长的数字不提供中文读法。
const MAX_INT_DIGITS: usize = 16;
const MAX_DEC_DIGITS: usize = 12;

/// 本地民用时间。宿主在按键前注入;纯引擎环境退回 UTC。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CivilTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// ISO 周几:1 = 星期一 … 7 = 星期日。
    pub weekday: u32,
}

impl CivilTime {
    /// 没有宿主注入时使用;E2E 全部注入固定时间,不受运行机器时区影响。
    pub(crate) fn now_utc() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        Self::from_unix(secs)
    }

    fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: (rem / 3_600) as u32,
            minute: (rem % 3_600 / 60) as u32,
            second: (rem % 60) as u32,
            // 1970-01-01 是星期四,加 3 后对 7 取余正好落在 ISO 星期一。
            weekday: ((days + 3).rem_euclid(7) + 1) as u32,
        }
    }
}

/// Howard Hinnant 的 civil_from_days:Unix 天数 → 公历年月日。
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

/// 当前原始串对应的快捷候选;没有触发时返回空。
pub fn candidates(raw: &str, now: CivilTime) -> Vec<Candidate> {
    if let Some(cands) = symbols::candidates(raw) {
        return cands;
    }
    if let Some((text, value)) = unicode_texts(raw) {
        let mut cands = wrap(vec![text], raw);
        if let Some(top) = cands.first_mut() {
            // 问字候选带码点提示,打字时能确认取的是哪个字符。
            top.hint = Some(format!("U+{value:04X}"));
        }
        return cands;
    }
    let texts = match raw {
        "rq" => date_texts(now),
        "sj" => time_texts(now),
        "xq" => weekday_texts(now),
        _ => expression_texts(raw)
            .or_else(|| circle_texts(raw))
            .unwrap_or_default(),
    };
    wrap(texts, raw)
}

/// 表达式模式允许的字符:十进制数字、四则运算符、括号、小数点和乘号 `x`。
pub(crate) fn is_expression_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '+' | '-' | '*' | '/' | '(' | ')' | '^' | '.' | 'x')
}

/// 需要写进组合的运算符;`x` 按字母键处理,小数点单独判断。
pub(crate) fn is_operator(c: char) -> bool {
    matches!(c, '+' | '-' | '*' | '/' | '(' | ')' | '^')
}

/// 全拼完整打出「日期/时间/星期」时,把对应写法插在这个词后面。
///
/// 只认整串候选(`full`),并且只扫前五条;匹配词本身仍按正常候选学习。
pub(crate) fn keyword(
    cands: &[Candidate],
    raw: &str,
    now: CivilTime,
) -> Option<(usize, Vec<Candidate>)> {
    const SCAN: usize = 5;
    let (index, texts) = cands
        .iter()
        .take(SCAN)
        .enumerate()
        .find_map(|(index, cand)| {
            if !cand.full {
                return None;
            }
            let texts = match cand.text.as_str() {
                "日期" => date_texts(now),
                "时间" => time_texts(now),
                "星期" => weekday_texts(now),
                _ => return None,
            };
            Some((index, texts))
        })?;
    let texts: Vec<String> = texts
        .into_iter()
        .filter(|text| !cands.iter().any(|cand| cand.text.as_str() == text.as_str()))
        .collect();
    if texts.is_empty() {
        return None;
    }
    Some((index, wrap(texts, raw)))
}

fn wrap(texts: Vec<String>, raw: &str) -> Vec<Candidate> {
    texts
        .into_iter()
        .enumerate()
        // 注入后不再排序;递减分只用于稳定顺序与诊断。
        .map(|(index, text)| Candidate::quick(raw.len(), text, 1.0e9 - index as f64))
        .collect()
}

fn date_texts(now: CivilTime) -> Vec<String> {
    vec![
        format!("{}年{}月{}日", now.year, now.month, now.day),
        format!("{:04}-{:02}-{:02}", now.year, now.month, now.day),
        format!("{:04}/{:02}/{:02}", now.year, now.month, now.day),
        format!("{}月{}日", now.month, now.day),
    ]
}

fn time_texts(now: CivilTime) -> Vec<String> {
    let mut out = vec![
        format!("{:02}:{:02}", now.hour, now.minute),
        format!("{:02}:{:02}:{:02}", now.hour, now.minute, now.second),
        format!("{}点{:02}分", now.hour, now.minute),
    ];
    let (label, hour) = match now.hour {
        0..=5 => ("凌晨", now.hour),
        6..=11 => ("上午", now.hour),
        12 => ("中午", 12),
        13..=23 => ("下午", now.hour - 12),
        _ => ("", now.hour),
    };
    out.push(format!("{label}{hour}:{:02}", now.minute));
    out.push(format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    ));
    out
}

fn weekday_texts(now: CivilTime) -> Vec<String> {
    const NAMES: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    let index = (now.weekday.clamp(1, 7) - 1) as usize;
    let mut out = vec![
        format!("星期{}", NAMES[index]),
        format!("周{}", NAMES[index]),
    ];
    // 口语里星期日、周日也常说星期天、周天。
    if index == 6 {
        out.push("星期天".to_string());
        out.push("周天".to_string());
    }
    out
}

/// 当前串会不会产生快捷候选。查询层用它跳过错音纠正,保证触发键秒出。
pub(crate) fn is_trigger(raw: &str) -> bool {
    matches!(raw, "rq" | "sj" | "xq")
        || symbols::is_trigger(raw)
        || unicode_body(raw).is_some()
        || number_body(raw).is_some()
}

/// `v` + 十进制数 → 小写/大写读法、金额与逐位读法。
fn number_texts(raw: &str) -> Option<Vec<String>> {
    let (int_part, dec_part) = number_body(raw)?;
    let mut texts = Vec::new();
    push_unique(&mut texts, chinese_number(int_part, dec_part, false));
    push_unique(&mut texts, chinese_number(int_part, dec_part, true));
    if dec_part.len() <= 2 {
        push_unique(&mut texts, amount_text(int_part, dec_part, false));
        push_unique(&mut texts, amount_text(int_part, dec_part, true));
    }
    // 逐位读法只对整数有意义;小数的「点」后本来就是逐位读。
    if dec_part.is_empty() {
        push_unique(&mut texts, digits_text(int_part));
    }
    // 1–20 追加圆圈数字;带小数与超长整数不出。
    if dec_part.is_empty()
        && int_part.len() <= 3
        && let Ok(n) = int_part.parse::<u32>()
        && (1..=20).contains(&n)
    {
        push_unique(&mut texts, circle_number(n));
    }
    Some(texts)
}

/// 圆圈数字 ①–⑳。
fn circle_number(n: u32) -> String {
    char::from_u32(0x2460 + n - 1)
        .unwrap_or('\u{2460}')
        .to_string()
}

/// `yuanquan` + 数字拼音 → 圆圈数字 ①–⑳;只认完整匹配,留给词典的
/// 「圆圈」不受影响。
pub(crate) fn circle_texts(raw: &str) -> Option<Vec<String>> {
    const PINYINS: [&str; 20] = [
        "yi", "er", "san", "si", "wu", "liu", "qi", "ba", "jiu", "shi", "shiyi", "shier",
        "shisan", "shisi", "shiwu", "shiliu", "shiqi", "shiba", "shijiu", "ershi",
    ];
    let rest = raw.strip_prefix("yuanquan")?;
    let index = PINYINS.iter().position(|p| *p == rest)?;
    Some(vec![circle_number(index as u32 + 1)])
}

fn push_unique(texts: &mut Vec<String>, text: String) {
    if !texts.contains(&text) {
        texts.push(text);
    }
}

/// `v` + 合法十进制数 → (整数部分, 小数部分);末尾刚敲的点按整数处理。
fn number_body(raw: &str) -> Option<(&str, &str)> {
    let body = raw.strip_prefix('v')?;
    // 末尾刚敲的点按整数处理;已经有一个小数点时,第二个点不吞掉。
    let body = match body.strip_suffix('.') {
        Some(rest) if !rest.contains('.') => rest,
        _ => body,
    };
    let bytes = body.as_bytes();
    if bytes.is_empty() || body.ends_with('.') || !bytes[0].is_ascii_digit() {
        return None;
    }
    let mut dots = 0;
    for &b in bytes {
        match b {
            b'0'..=b'9' => {}
            b'.' => {
                dots += 1;
                if dots > 1 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let (int_part, dec_part) = body.split_once('.').unwrap_or((body, ""));
    if int_part.len() > MAX_INT_DIGITS || dec_part.len() > MAX_DEC_DIGITS {
        return None;
    }
    Some((int_part, dec_part))
}

/// u 问字模式下的码点正文:至多六位十六进制,可带前导 `+`;空串也算还在输入。
pub(crate) fn unicode_body(raw: &str) -> Option<&str> {
    let body = raw.strip_prefix('u')?;
    let hex = body.strip_prefix('+').unwrap_or(body);
    (hex.len() <= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(body)
}

/// `u4e00` / `u+1f600` → 字符与码点;纯字母不带 `+` 仍留给拼音。
fn unicode_texts(raw: &str) -> Option<(String, u32)> {
    let body = unicode_body(raw)?;
    let (explicit, hex) = match body.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    if hex.is_empty()
        || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        || (!explicit && !hex.bytes().any(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let c = char::from_u32(value)?;
    (!c.is_control()).then(|| (c.to_string(), value))
}

/// `v` 后面的内容:纯数字出数字读法,四位年份加月日出日期,其余当四则算式。
fn expression_texts(raw: &str) -> Option<Vec<String>> {
    if let Some(texts) = number_texts(raw) {
        return Some(texts);
    }
    let body = raw.strip_prefix('v')?;
    if let Some(texts) = input_date_texts(body) {
        return Some(texts);
    }
    // 没有运算符就不是算式;纯数字超长时宁可不给候选,也不出「数字=数字」。
    if !body
        .bytes()
        .any(|b| matches!(b, b'+' | b'-' | b'*' | b'/' | b'(' | b')' | b'^' | b'x'))
    {
        return None;
    }
    let result = evaluate(body)?;
    Some(vec![result.clone(), format!("{body}={result}")])
}

/// `1998-3-8` / `1998/3/8` / `1998.3.8` → 中文日期、年月日与补零日期。
fn input_date_texts(body: &str) -> Option<Vec<String>> {
    let mut parts = body.split(['-', '/', '.']);
    let year = parts.next()?;
    let month = parse_date_part(parts.next()?)?;
    let day = parse_date_part(parts.next()?)?;
    if parts.next().is_some()
        || year.len() != 4
        || !year.bytes().all(|b| b.is_ascii_digit())
        || !(1..=12).contains(&month)
    {
        return None;
    }
    let year_number: i32 = year.parse().ok()?;
    if year_number <= 0 || day == 0 || day > days_in_month(year_number, month) {
        return None;
    }
    let year_text: String = year
        .bytes()
        .map(|b| CHAR_DIGITS[usize::from(b - b'0')])
        .collect();
    Some(vec![
        format!(
            "{year_text}年{}月{}日",
            numeral(&month.to_string(), false),
            numeral(&day.to_string(), false)
        ),
        format!("{year}年{month}月{day}日"),
        format!("{year}-{month:02}-{day:02}"),
    ])
}

/// 月日最多两位数字,不接受符号或前导 `+`。
fn parse_date_part(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// 四则算式求值:支持 `+ - * x / ^`、括号、小数与一元正负号。
///
/// 语法错、除零或结果溢出都返回 `None`;结果按整数或最多十位小数输出。
fn evaluate(text: &str) -> Option<String> {
    let mut evaluator = Evaluator {
        bytes: text.as_bytes(),
        position: 0,
    };
    let value = evaluator.expression()?;
    if evaluator.position != evaluator.bytes.len() || !value.is_finite() {
        return None;
    }
    Some(format_number(value))
}

struct Evaluator<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Evaluator<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    /// 加减。
    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.position += 1;
            let rhs = self.term()?;
            value = if op == b'+' { value + rhs } else { value - rhs };
        }
        Some(value)
    }

    /// 乘除;键盘上打 `*` 要按 Shift,所以 `x` 也当乘号。
    fn term(&mut self) -> Option<f64> {
        let mut value = self.power()?;
        while let Some(op @ (b'*' | b'x' | b'/')) = self.peek() {
            self.position += 1;
            let rhs = self.power()?;
            value = if op == b'/' {
                if rhs == 0.0 {
                    return None;
                }
                value / rhs
            } else {
                value * rhs
            };
        }
        Some(value)
    }

    /// 乘方,右结合。
    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.peek() == Some(b'^') {
            self.position += 1;
            let exponent = self.power()?;
            return Some(base.powf(exponent));
        }
        Some(base)
    }

    /// 一元正负号。
    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(b'-') => {
                self.position += 1;
                self.unary().map(|value| -value)
            }
            Some(b'+') => {
                self.position += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    /// 数字或括号。
    fn atom(&mut self) -> Option<f64> {
        if self.peek() == Some(b'(') {
            self.position += 1;
            let value = self.expression()?;
            if self.peek() != Some(b')') {
                return None;
            }
            self.position += 1;
            return Some(value);
        }
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.')) {
            self.position += 1;
        }
        if start == self.position {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.position])
            .ok()?
            .parse()
            .ok()
    }
}

/// 整数不带小数点;小数最多十位并去掉末尾的零。
fn format_number(value: f64) -> String {
    if value.abs() < f64::MIN_POSITIVE {
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let text = format!("{value:.10}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

fn chinese_number(int_part: &str, dec_part: &str, upper: bool) -> String {
    let mut text = numeral(int_part, upper);
    if !dec_part.is_empty() {
        let digits = if upper { &UPPER_DIGITS } else { &LOWER_DIGITS };
        text.push('点');
        for b in dec_part.bytes() {
            text.push_str(digits[usize::from(b - b'0')]);
        }
    }
    text
}

/// 位值读法;小写省掉开头的「一十」,财务大写保留「壹拾」。
fn numeral(int_part: &str, upper: bool) -> String {
    let (digits, units) = if upper {
        (&UPPER_DIGITS, &UPPER_UNITS)
    } else {
        (&LOWER_DIGITS, &LOWER_UNITS)
    };
    let mut text = number_to_chinese(int_part, digits, units);
    if !upper && text.starts_with("一十") {
        text.replace_range(.."一十".len(), "十");
    }
    text
}

/// 金额:元后无角分写「整」;有分无角补「零」;不足一元不写元。
fn amount_text(int_part: &str, dec_part: &str, upper: bool) -> String {
    let digits = if upper { &UPPER_DIGITS } else { &LOWER_DIGITS };
    let jiao = dec_part
        .as_bytes()
        .first()
        .map_or(0, |b| usize::from(b - b'0'));
    let fen = dec_part
        .as_bytes()
        .get(1)
        .map_or(0, |b| usize::from(b - b'0'));
    let yuan = int_part.trim_start_matches('0');
    let mut text = String::new();
    if !yuan.is_empty() || (jiao == 0 && fen == 0) {
        text.push_str(&numeral(int_part, upper));
        text.push('元');
    }
    if jiao == 0 && fen == 0 {
        text.push('整');
        return text;
    }
    if jiao > 0 {
        text.push_str(digits[jiao]);
        text.push('角');
    } else if !yuan.is_empty() {
        text.push_str(digits[0]);
    }
    if fen > 0 {
        text.push_str(digits[fen]);
        text.push('分');
    }
    text
}

/// 逐位读法,保留前导零;年份、编号常用「二〇二六」。
fn digits_text(int_part: &str) -> String {
    let mut text = String::new();
    for b in int_part.bytes() {
        text.push_str(CHAR_DIGITS[usize::from(b - b'0')]);
    }
    text
}

/// 整数按「亿」递归拆分,亿以内再按四位一节读。
fn number_to_chinese(num: &str, digits: &[&str; 10], units: &[&str; 4]) -> String {
    let trimmed = num.trim_start_matches('0');
    if trimmed.is_empty() {
        return digits[0].to_string();
    }
    below_yi_or_split(trimmed, digits, units)
}

/// 超过八位时,高位整体读出后接「亿」;低八位不足八位时补一个「零」。
fn below_yi_or_split(num: &str, digits: &[&str; 10], units: &[&str; 4]) -> String {
    if num.len() <= 8 {
        return below_yi(num, digits, units);
    }
    let (high, low) = num.split_at(num.len() - 8);
    let mut text = below_yi_or_split(high, digits, units);
    text.push('亿');
    let low = low.trim_start_matches('0');
    if !low.is_empty() {
        if low.len() < 8 {
            text.push_str(digits[0]);
        }
        text.push_str(&below_yi(low, digits, units));
    }
    text
}

/// 亿以内(最多八位,无前导零):从低位起每四位一节。
fn below_yi(num: &str, digits: &[&str; 10], units: &[&str; 4]) -> String {
    let mut groups: Vec<&str> = Vec::new();
    let mut end = num.len();
    while end > 0 {
        let start = end.saturating_sub(4);
        groups.push(&num[start..end]);
        end = start;
    }
    let mut text = String::new();
    for (index, group) in groups.iter().enumerate().rev() {
        let group_text = group_to_chinese(group, digits, units);
        if group_text.is_empty() {
            continue;
        }
        if !text.is_empty() && needs_leading_zero(group) {
            text.push_str(digits[0]);
        }
        text.push_str(&group_text);
        if let Some(unit) = GROUP_UNITS.get(index) {
            text.push_str(unit);
        }
    }
    if text.is_empty() {
        digits[0].to_string()
    } else {
        text
    }
}

/// 组内是否要在前面补「零」:四位组以 0 开头,或高位组不足四位。
fn needs_leading_zero(group: &str) -> bool {
    group.len() < 4 || group.starts_with('0')
}

fn group_to_chinese(group: &str, digits: &[&str; 10], units: &[&str; 4]) -> String {
    let mut text = String::new();
    let mut pending_zero = false;
    let length = group.len();
    for (index, b) in group.bytes().enumerate() {
        let digit = usize::from(b - b'0');
        if digit == 0 {
            pending_zero = true;
            continue;
        }
        if pending_zero && !text.is_empty() {
            text.push_str(digits[0]);
        }
        pending_zero = false;
        text.push_str(digits[digit]);
        if let Some(unit) = units.get(length - 1 - index) {
            text.push_str(unit);
        }
    }
    text
}
