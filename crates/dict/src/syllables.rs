//! 合法拼音音节全集与简拼、模糊音工具。
//!
//! 约定:全部 ASCII,ü 一律写作 `v`(`lv`/`nv`/`lve`/`nve`),与白霜词库的注音习惯一致。

use std::collections::HashSet;
use std::sync::OnceLock;

/// 零声母与各声母的全部合法音节(按书写形式)。
pub const SYLLABLES: &[&str] = &[
    // 零声母
    "a", "ai", "an", "ang", "ao", "e", "ei", "en", "er", "o", "ou",
    // b
    "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi", "bian", "biao", "bie", "bin",
    "bing", "bo", "bu",
    // c
    "ca", "cai", "can", "cang", "cao", "ce", "cen", "ceng", "ci", "cong", "cou", "cu", "cuan",
    "cui", "cun", "cuo",
    // ch
    "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong", "chou", "chu",
    "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo",
    // d
    "da", "dai", "dan", "dang", "dao", "de", "dei", "den", "dia", "dian", "diao", "die", "ding",
    "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo",
    // f
    "fa", "fan", "fang", "fei", "fen", "feng", "fo", "fou", "fu",
    // g
    "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua",
    "guai", "guan", "guang", "gui", "gun", "guo",
    // h
    "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua",
    "huai", "huan", "huang", "hui", "hun", "huo",
    // j
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan",
    "jue", "jun",
    // k
    "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong", "kou", "ku", "kua",
    "kuai", "kuan", "kuang", "kui", "kun", "kuo",
    // l
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao",
    "lie", "lin", "ling", "liu", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    // m
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie",
    "min", "ming", "mo", "mou", "mu",
    // n
    "na", "nai", "nan", "nao", "ne", "nei", "nen", "nang", "ni", "nian", "niang", "niao", "nie",
    "nin", "ning", "niu", "nong", "nu", "nun", "nuan", "nuo", "nv", "nve",
    // p
    "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin",
    "ping", "po", "pu",
    // q
    "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan",
    "que", "qun",
    // r
    "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "rua", "ruan", "run",
    "ruo",
    // s
    "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "si", "song", "sou", "su", "suan",
    "sui", "sun", "suo",
    // sh
    "sha", "shai", "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu",
    "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo",
    // t
    "ta", "tai", "tan", "tang", "tao", "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting",
    "tong", "tou", "tu", "tuan", "tui", "tun", "tuo",
    // w
    "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu",
    // x
    "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan",
    "xue", "xun",
    // y
    "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan",
    "yun",
    // z
    "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zi", "zong", "zou", "zu",
    "zuan", "zui", "zun", "zuo",
    // zh
    "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong",
    "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo",
];

/// 能做简拼首字母的字母:所有辅音声母字母加 a/e/o(`i`/`u`/`v` 不能起音节)。
pub const ABBREV_LETTERS: &str = "abcdfghjklmnpqrstwxyz";

/// 能整体做简拼的双字母声母。
pub const ABBREV_DIGRAPHS: &[&str] = &["zh", "ch", "sh"];

fn set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| SYLLABLES.iter().copied().collect())
}

/// 是否合法全拼音节。
pub fn is_syllable(s: &str) -> bool {
    set().contains(&s)
}

/// 单字母是否可作简拼。
pub fn is_abbrev_letter(c: char) -> bool {
    c.is_ascii_lowercase() && ABBREV_LETTERS.contains(c)
}

/// 字符串是否是简拼单元(单字母或 zh/ch/sh)。
pub fn is_abbrev(s: &str) -> bool {
    ABBREV_DIGRAPHS.contains(&s) || (s.chars().count() == 1 && s.chars().all(is_abbrev_letter))
}

/// 模糊音开关集合,默认全关。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fuzzy {
    pub zcs: bool,     // z↔zh、c↔ch、s↔sh
    pub ln: bool,      // l↔n
    pub fh: bool,      // f↔h
    pub an_ang: bool,  // an↔ang
    pub en_eng: bool,  // en↔eng
    pub in_ing: bool,  // in↔ing
}

/// 把音节折叠到等价类代表形:zh→z、ch→c、sh→s、n→l、f→h、ang→an、eng→en、ing→in。
fn canonical(s: &str, fuzzy: &Fuzzy) -> String {
    let mut t = s;
    let mut head = String::new();
    if fuzzy.zcs {
        for d in ABBREV_DIGRAPHS {
            if let Some(rest) = t.strip_prefix(d) {
                head.push(d.as_bytes()[0] as char);
                t = rest;
                break;
            }
        }
    }
    if t == s && fuzzy.ln && t.starts_with('n') {
        head.push('l');
        t = &t[1..];
    }
    if t == s && fuzzy.fh && t.starts_with('f') {
        head.push('h');
        t = &t[1..];
    }
    let mut out = head;
    if fuzzy.an_ang {
        if let Some(rest) = t.strip_suffix("ang") {
            out.push_str(rest);
            out.push_str("an");
            return out;
        }
    }
    if fuzzy.en_eng {
        if let Some(rest) = t.strip_suffix("eng") {
            out.push_str(rest);
            out.push_str("en");
            return out;
        }
    }
    if fuzzy.in_ing {
        if let Some(rest) = t.strip_suffix("ing") {
            out.push_str(rest);
            out.push_str("in");
            return out;
        }
    }
    out.push_str(t);
    out
}

/// 一个音节在给定模糊音配置下的等价音节集合(含自身)。简拼串原样返回。
pub fn expansions(s: &str, fuzzy: &Fuzzy) -> Vec<String> {
    if !is_syllable(s) {
        return vec![s.to_string()];
    }
    let c = canonical(s, fuzzy);
    SYLLABLES
        .iter()
        .filter(|&&sy| canonical(sy, fuzzy) == c)
        .map(|sy| sy.to_string())
        .collect()
}
