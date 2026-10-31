//! 合法拼音音节全集与简拼、模糊音工具。
//!
//! 约定:全部 ASCII,词库与学习键里的 ü 一律写作 `v`(`lv`/`nv`/`lve`/`nve`),
//! 与白霜词库的注音习惯一致;输入另接受 `lue`/`nue` 作为 `lve`/`nve` 的别名。

use std::collections::HashSet;
use std::sync::OnceLock;

/// 零声母与各声母的全部合法音节(按书写形式)。
pub const SYLLABLES: &[&str] = &[
    // 零声母
    "a", "ai", "an", "ang", "ao", "e", "ei", "en", "er", "o", "ou", // b
    "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi", "bian", "biao", "bie", "bin",
    "bing", "bo", "bu", // c
    "ca", "cai", "can", "cang", "cao", "ce", "cen", "ceng", "ci", "cong", "cou", "cu", "cuan",
    "cui", "cun", "cuo", // ch
    "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong", "chou", "chu",
    "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", // d
    "da", "dai", "dan", "dang", "dao", "de", "dei", "den", "deng", "di", "dia", "dian", "diao",
    "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo", // f
    "fa", "fan", "fang", "fei", "fen", "feng", "fo", "fou", "fu", // g
    "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua",
    "guai", "guan", "guang", "gui", "gun", "guo", // h
    "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong", "hou", "hu", "hua",
    "huai", "huan", "huang", "hui", "hun", "huo", // j
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan",
    "jue", "jun", // k
    "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong", "kou", "ku", "kua",
    "kuai", "kuan", "kuang", "kui", "kun", "kuo", // l
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia", "lian", "liang", "liao",
    "lie", "lin", "ling", "liu", "lo", "long", "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    // m
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi", "mian", "miao", "mie",
    "min", "ming", "miu", "mo", "mou", "mu", // n
    "na", "nai", "nan", "nao", "ne", "nei", "nen", "neng", "nang", "ni", "nian", "niang", "niao",
    "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nun", "nuan", "nuo", "nv", "nve", // p
    "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian", "piao", "pie", "pin",
    "ping", "po", "pou", "pu", // q
    "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan",
    "que", "qun", // r
    "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "rua", "ruan", "rui",
    "run", "ruo", // s
    "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "si", "song", "sou", "su", "suan",
    "sui", "sun", "suo", // sh
    "sha", "shai", "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu",
    "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo", // t
    "ta", "tai", "tan", "tang", "tao", "te", "tei", "teng", "ti", "tian", "tiao", "tie", "ting",
    "tong", "tou", "tu", "tuan", "tui", "tun", "tuo", // w
    "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu", // x
    "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan",
    "xue", "xun", // y
    "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu", "yuan",
    "yue", "yun", // z
    "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zi", "zong", "zou", "zu",
    "zuan", "zui", "zun", "zuo", // zh
    "zha", "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou",
    "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo",
];

/// 能做简拼首字母的字母:所有辅音声母字母加 a/e/o(`i`/`u`/`v` 不能起音节)。
pub const ABBREV_LETTERS: &str = "abcdfghjklmnpqrstwxyz";

/// 能整体做简拼的双字母声母。
pub const ABBREV_DIGRAPHS: &[&str] = &["zh", "ch", "sh"];

/// 模糊组位掩码。每组一个 bit,候选可同时命中多组,组级学习按位累加。
pub const GROUP_ZCS: u8 = 1 << 0;
pub const GROUP_LN: u8 = 1 << 1;
pub const GROUP_FH: u8 = 1 << 2;
pub const GROUP_RL: u8 = 1 << 3;
pub const GROUP_AN_ANG: u8 = 1 << 4;
pub const GROUP_EN_ENG: u8 = 1 << 5;
pub const GROUP_IN_ING: u8 = 1 << 6;

/// 组名与位掩码一一对应;持久化与设置页统计共用这一份顺序。
pub const FUZZY_GROUPS: [&str; 7] = ["zcs", "ln", "fh", "rl", "an_ang", "en_eng", "in_ing"];

fn set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| SYLLABLES.iter().copied().collect())
}

/// ü 的常见输入别名。词库与学习键统一用 `v`,但用户习惯把 ü 打成 `ue`,
/// 两种写法要落到同一个音节上。
pub fn canonical(s: &str) -> &str {
    match s {
        "lue" => "lve",
        "nue" => "nve",
        _ => s,
    }
}

/// 是否合法全拼音节。`lue`/`nue` 作为 `lve`/`nve` 的别名同样合法。
pub fn is_syllable(s: &str) -> bool {
    set().contains(&s) || matches!(s, "lue" | "nue")
}

/// 模糊音开关集合。引擎默认用 [`Self::auto`] 全开核心组;`off` 只留给
/// `config.toml` 的排障开关与自动化测试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuzzy {
    pub zcs: bool,    // z↔zh、c↔ch、s↔sh
    pub ln: bool,     // l↔n
    pub fh: bool,     // f↔h
    pub rl: bool,     // r↔l
    pub an_ang: bool, // an↔ang(含 ian↔iang、uan↔uang)
    pub en_eng: bool, // en↔eng
    pub in_ing: bool, // in↔ing
}

impl Fuzzy {
    /// 全关。`Default` 保持全关,方便测试明确表达「关闭」。
    pub const fn off() -> Self {
        Self {
            zcs: false,
            ln: false,
            fh: false,
            rl: false,
            an_ang: false,
            en_eng: false,
            in_ing: false,
        }
    }

    /// 核心模糊组全开。用户不再逐项配置,排序由处数罚分与学习覆盖负责。
    pub const fn auto() -> Self {
        Self {
            zcs: true,
            ln: true,
            fh: true,
            rl: true,
            an_ang: true,
            en_eng: true,
            in_ing: true,
        }
    }

    pub const fn any(&self) -> bool {
        self.zcs || self.ln || self.fh || self.rl || self.an_ang || self.en_eng || self.in_ing
    }

    pub const fn is_off(&self) -> bool {
        !self.any()
    }
}

impl Default for Fuzzy {
    fn default() -> Self {
        Self::off()
    }
}

/// 拆声母与韵母。双字母声母按 zh/ch/sh 处理,其余辅音单字母,零声母整体算韵母。
fn split_initial(s: &str) -> (&str, &str) {
    let len = if s.starts_with("zh") || s.starts_with("ch") || s.starts_with("sh") {
        2
    } else if s
        .chars()
        .next()
        .is_some_and(|c| "bpmfdtnlgkhjqxrzcsyw".contains(c))
    {
        1
    } else {
        0
    };
    s.split_at(len)
}

/// 声母的直接等价形(含自身)。按规则两两展开,不做传递闭包:
/// 同时开 n/l 与 r/l 时,l 可到 n 或 r,n 与 r 仍互相独立。
fn initial_variants(initial: &str, fuzzy: &Fuzzy) -> Vec<(String, u8)> {
    let mut out = vec![(initial.to_string(), 0)];
    if fuzzy.zcs {
        match initial {
            "zh" => out.push(("z".into(), GROUP_ZCS)),
            "z" => out.push(("zh".into(), GROUP_ZCS)),
            "ch" => out.push(("c".into(), GROUP_ZCS)),
            "c" => out.push(("ch".into(), GROUP_ZCS)),
            "sh" => out.push(("s".into(), GROUP_ZCS)),
            "s" => out.push(("sh".into(), GROUP_ZCS)),
            _ => {}
        }
    }
    if fuzzy.ln {
        match initial {
            "n" => out.push(("l".into(), GROUP_LN)),
            "l" => out.push(("n".into(), GROUP_LN)),
            _ => {}
        }
    }
    if fuzzy.fh {
        match initial {
            "f" => out.push(("h".into(), GROUP_FH)),
            "h" => out.push(("f".into(), GROUP_FH)),
            _ => {}
        }
    }
    if fuzzy.rl {
        match initial {
            "r" => out.push(("l".into(), GROUP_RL)),
            "l" => out.push(("r".into(), GROUP_RL)),
            _ => {}
        }
    }
    out
}

/// 韵母的直接等价形(含自身)。后缀替换覆盖 an/ang、ian/iang、uan/uang
/// 与 en/eng、in/ing。
fn final_variants(final_part: &str, fuzzy: &Fuzzy) -> Vec<(String, u8)> {
    let mut out = vec![(final_part.to_string(), 0)];
    if fuzzy.an_ang {
        if let Some(rest) = final_part.strip_suffix("ang") {
            out.push((format!("{rest}an"), GROUP_AN_ANG));
        } else if let Some(rest) = final_part.strip_suffix("an") {
            out.push((format!("{rest}ang"), GROUP_AN_ANG));
        }
    }
    if fuzzy.en_eng {
        if let Some(rest) = final_part.strip_suffix("eng") {
            out.push((format!("{rest}en"), GROUP_EN_ENG));
        } else if let Some(rest) = final_part.strip_suffix("en") {
            out.push((format!("{rest}eng"), GROUP_EN_ENG));
        }
    }
    if fuzzy.in_ing {
        if let Some(rest) = final_part.strip_suffix("ing") {
            out.push((format!("{rest}in"), GROUP_IN_ING));
        } else if let Some(rest) = final_part.strip_suffix("in") {
            out.push((format!("{rest}ing"), GROUP_IN_ING));
        }
    }
    out
}

/// 一个音节的模糊等价音节、改动处数与命中的组。自身在首位(处数 0)。
/// 简拼串原样返回。声母、韵母各算一处:改声母或改韵母为 1,两者都改为 2。
pub fn expansions_scored(s: &str, fuzzy: &Fuzzy) -> Vec<(String, usize, u8)> {
    let s = canonical(s);
    if !is_syllable(s) || fuzzy.is_off() {
        return vec![(s.to_string(), 0, 0)];
    }
    let (initial, final_part) = split_initial(s);
    let initials = initial_variants(initial, fuzzy);
    let finals = final_variants(final_part, fuzzy);
    let mut out = Vec::with_capacity(initials.len() * finals.len());
    for (i, initial_group) in &initials {
        for (f, final_group) in &finals {
            let mut variant = String::with_capacity(i.len() + f.len());
            variant.push_str(i);
            variant.push_str(f);
            if variant.as_str() != s && !is_syllable(&variant) {
                continue;
            }
            let edits = usize::from(i.as_str() != initial) + usize::from(f.as_str() != final_part);
            out.push((variant, edits, *initial_group | *final_group));
        }
    }
    out
}

/// 某个首字母在开启的模糊组下的等价首字母及命中组(含自身,组为 0)。
/// `l` 在 n/l 与 r/l 同时开启时得到 `{l, n, r}`,是唯一一个三元素等价集。
pub fn initial_alternatives_scored(c: char, fuzzy: &Fuzzy) -> Vec<(char, u8)> {
    let mut out = vec![(c, 0)];
    if fuzzy.ln {
        match c {
            'n' => out.push(('l', GROUP_LN)),
            'l' => out.push(('n', GROUP_LN)),
            _ => {}
        }
    }
    if fuzzy.fh {
        match c {
            'f' => out.push(('h', GROUP_FH)),
            'h' => out.push(('f', GROUP_FH)),
            _ => {}
        }
    }
    if fuzzy.rl {
        match c {
            'r' => out.push(('l', GROUP_RL)),
            'l' => out.push(('r', GROUP_RL)),
            _ => {}
        }
    }
    out
}

/// 某个首字母在开启的模糊组下的等价首字母(含自身)。
pub fn initial_alternatives(c: char, fuzzy: &Fuzzy) -> Vec<char> {
    initial_alternatives_scored(c, fuzzy)
        .into_iter()
        .map(|(c, _)| c)
        .collect()
}

/// 两个首字母相同或属于同一模糊组时返回命中的组;相同为 0。
/// 只做直接配对,不让 n/l 与 r/l 经 l 传递成 n/r。
pub fn initial_group(a: char, b: char, fuzzy: &Fuzzy) -> Option<u8> {
    if a == b {
        return Some(0);
    }
    if fuzzy.ln && ((a == 'n' && b == 'l') || (a == 'l' && b == 'n')) {
        return Some(GROUP_LN);
    }
    if fuzzy.fh && ((a == 'f' && b == 'h') || (a == 'h' && b == 'f')) {
        return Some(GROUP_FH);
    }
    if fuzzy.rl && ((a == 'r' && b == 'l') || (a == 'l' && b == 'r')) {
        return Some(GROUP_RL);
    }
    None
}
