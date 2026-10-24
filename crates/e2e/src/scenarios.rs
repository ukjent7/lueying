//! E2E 场景规格:每个场景列出「它可能怎么坏」。分阶段推进,阶段末逐条断言。
//! 全部基于内置迷你词库;主词典质量与性能探针见 [`crate::probes`]。

use pinyin_engine::{Key, Mode};

pub struct Scenario {
    pub name: &'static str,
    /// 该场景启用 z↔zh/c↔ch/s↔sh 模糊音。
    pub fuzzy: bool,
    pub stages: Vec<Stage>,
}

pub struct Stage {
    pub keys: Vec<Key>,
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// 从上一阶段末到本阶段末,按序上屏的文本。
    Commits(Vec<String>),
    /// 全程无上屏。
    NoCommits,
    Top(String),
    Contains(String),
    NotContains(String),
    /// 候选列表(精确)。
    Cands(Vec<String>),
    /// 组合原文。
    Raw(String),
    NoComp,
    /// 模式序列(含初始 Chinese)。
    ModeSeq(Vec<Mode>),
    /// 至少一个直通事件。
    Passthrough,
}

/// 字母串转按键(小写字母;数字字符转数字键)。
fn l(s: &str) -> Vec<Key> {
    s.chars()
        .map(|c| {
            if c.is_ascii_digit() {
                Key::Digit(c as u8 - b'0')
            } else {
                Key::Letter(c)
            }
        })
        .collect()
}

fn stage(keys: Vec<Key>, checks: Vec<Check>) -> Stage {
    Stage { keys, checks }
}

fn s(name: &'static str, fuzzy: bool, stages: Vec<Stage>) -> Scenario {
    Scenario {
        name,
        fuzzy,
        stages,
    }
}

fn commits(words: &[&str]) -> Check {
    Check::Commits(words.iter().map(|w| w.to_string()).collect())
}

pub fn scenarios() -> Vec<Scenario> {
    vec![
        s(
            "全拼词组+空格上屏",
            false,
            vec![
                stage(
                    l("nihao"),
                    vec![Check::Top("你好".into()), Check::NoCommits],
                ),
                stage(vec![Key::Space], vec![commits(&["你好"]), Check::NoComp]),
            ],
        ),
        s(
            "整句贪心转换",
            false,
            vec![
                stage(l("jintiantianqihenhao"), vec![]),
                stage(vec![Key::Space], vec![commits(&["今天天气很好"])]),
            ],
        ),
        s(
            "简拼 nh→你好",
            false,
            vec![
                stage(l("nh"), vec![Check::Top("你好".into())]),
                stage(vec![Key::Space], vec![commits(&["你好"])]),
            ],
        ),
        s(
            "隔音符消歧 xi'an→西安",
            false,
            vec![
                stage(
                    vec![
                        Key::Letter('x'),
                        Key::Letter('i'),
                        Key::Punct('\''),
                        Key::Letter('a'),
                        Key::Letter('n'),
                    ],
                    vec![Check::Top("西安".into())],
                ),
                stage(vec![Key::Space], vec![commits(&["西安"])]),
            ],
        ),
        s(
            "无隔音 xian→现",
            false,
            vec![
                stage(l("xian"), vec![Check::Contains("西安".into())]),
                stage(vec![Key::Space], vec![commits(&["现"])]),
            ],
        ),
        s(
            "整词 xianzai→现在",
            false,
            vec![
                stage(l("xianzai"), vec![]),
                stage(vec![Key::Space], vec![commits(&["现在"])]),
            ],
        ),
        s(
            "Backspace按音节回删",
            false,
            vec![
                stage(
                    vec![
                        Key::Letter('n'),
                        Key::Letter('i'),
                        Key::Letter('h'),
                        Key::Letter('a'),
                        Key::Letter('o'),
                        Key::Backspace,
                    ],
                    vec![Check::Raw("ni".into())],
                ),
                stage(vec![Key::Space], vec![commits(&["你"]), Check::NoComp]),
            ],
        ),
        s(
            "空组合态Backspace直通",
            false,
            vec![stage(
                vec![Key::Backspace],
                vec![Check::Passthrough, Check::NoCommits, Check::NoComp],
            )],
        ),
        s(
            "Enter上屏原文",
            false,
            vec![
                stage(l("nihao"), vec![]),
                stage(vec![Key::Enter], vec![commits(&["nihao"]), Check::NoComp]),
            ],
        ),
        s(
            "Esc清空后重新起组",
            false,
            vec![
                stage(l("nihao"), vec![]),
                stage(vec![Key::Esc], vec![Check::NoComp, Check::NoCommits]),
                stage(l("wo"), vec![]),
                stage(vec![Key::Space], vec![commits(&["我"])]),
            ],
        ),
        s(
            "数字选词",
            false,
            vec![
                stage(l("wo"), vec![Check::Top("我".into())]),
                stage(vec![Key::Digit(2)], vec![commits(&["握"])]),
            ],
        ),
        s(
            "翻页后选词",
            false,
            vec![
                stage(
                    vec![Key::Letter('y'), Key::Letter('i'), Key::PageDown],
                    vec![],
                ),
                stage(vec![Key::Digit(1)], vec![commits(&["易"])]),
            ],
        ),
        s(
            "Shift切英文直通",
            false,
            vec![
                stage(
                    vec![Key::Shift],
                    vec![Check::ModeSeq(vec![Mode::Chinese, Mode::English])],
                ),
                stage(
                    vec![Key::Letter('a'), Key::Punct(',')],
                    vec![Check::Passthrough, Check::NoCommits],
                ),
                stage(
                    vec![Key::Shift],
                    vec![Check::ModeSeq(vec![
                        Mode::Chinese,
                        Mode::English,
                        Mode::Chinese,
                    ])],
                ),
            ],
        ),
        s(
            "组合中Shift先上屏再切英",
            false,
            vec![
                stage(l("nihao"), vec![]),
                stage(
                    vec![Key::Shift],
                    vec![
                        commits(&["你好"]),
                        Check::ModeSeq(vec![Mode::Chinese, Mode::English]),
                    ],
                ),
            ],
        ),
        s(
            "组合中标点=上屏首选+全角标点",
            false,
            vec![
                stage(l("nihao"), vec![]),
                stage(vec![Key::Punct(',')], vec![commits(&["你好", "，"])]),
            ],
        ),
        s(
            "中文标点全表(含引号交替)",
            false,
            vec![stage(
                vec![
                    Key::Punct(','),
                    Key::Punct('.'),
                    Key::Punct('!'),
                    Key::Punct('?'),
                    Key::Punct(';'),
                    Key::Punct(':'),
                    Key::Punct('('),
                    Key::Punct(')'),
                    Key::Punct('<'),
                    Key::Punct('>'),
                    Key::Punct('\\'),
                    Key::Punct('"'),
                    Key::Punct('"'),
                    Key::Punct('\''),
                    Key::Punct('\''),
                ],
                vec![commits(&[
                    "，", "。", "！", "？", "；", "：", "（", "）", "〈", "〉", "、", "“", "”",
                    "‘", "’",
                ])],
            )],
        ),
        s(
            "英文态标点直通",
            false,
            vec![
                stage(vec![Key::Shift], vec![]),
                stage(
                    vec![Key::Punct(',')],
                    vec![Check::Passthrough, Check::NoCommits],
                ),
            ],
        ),
        s(
            "选词学习提频",
            false,
            vec![
                stage(
                    vec![Key::Letter('s'), Key::Letter('h'), Key::Letter('i')],
                    vec![Check::Top("是".into())],
                ),
                stage(vec![Key::Digit(2)], vec![commits(&["事"])]),
                stage(l("shi"), vec![Check::Top("事".into())]),
                stage(vec![Key::Space], vec![commits(&["事"])]),
            ],
        ),
        s(
            "前缀词逐段上屏",
            false,
            vec![
                stage(l("jintiantianqi"), vec![Check::Top("今天天气".into())]),
                stage(
                    vec![Key::Digit(2)],
                    vec![commits(&["今天"]), Check::Raw("tianqi".into())],
                ),
                stage(vec![Key::Space], vec![commits(&["天气"]), Check::NoComp]),
            ],
        ),
        s(
            "非法起始字母不崩溃",
            false,
            vec![
                stage(
                    vec![Key::Letter('i')],
                    vec![Check::Raw("i".into()), Check::Cands(vec![])],
                ),
                stage(vec![Key::Esc], vec![Check::NoComp]),
            ],
        ),
        s(
            "Home+Delete前删音节",
            false,
            vec![stage(
                vec![
                    Key::Letter('j'),
                    Key::Letter('i'),
                    Key::Letter('n'),
                    Key::Letter('t'),
                    Key::Letter('i'),
                    Key::Letter('a'),
                    Key::Letter('n'),
                    Key::Home,
                    Key::Delete,
                ],
                vec![Check::Raw("tian".into())],
            )],
        ),
        s(
            "模糊音默认关:si不含shi的字",
            false,
            vec![stage(
                l("si"),
                vec![Check::Top("四".into()), Check::NotContains("是".into())],
            )],
        ),
        s(
            "模糊音开:si命中shi的字",
            true,
            vec![stage(
                l("si"),
                vec![Check::Top("是".into()), Check::Contains("四".into())],
            )],
        ),
        s(
            "长简拼 zhrmghg→中华人民共和国",
            false,
            vec![
                stage(l("zhrmghg"), vec![Check::Top("中华人民共和国".into())]),
                stage(vec![Key::Space], vec![commits(&["中华人民共和国"])]),
            ],
        ),
        s(
            "缓冲上限40",
            false,
            vec![stage(
                l(&"a".repeat(41)),
                vec![Check::Raw("a".repeat(40)), Check::Cands(vec![])],
            )],
        ),
        s(
            "方向键按音节移动+回删",
            false,
            vec![
                stage(
                    vec![
                        Key::Letter('n'),
                        Key::Letter('i'),
                        Key::Letter('h'),
                        Key::Letter('a'),
                        Key::Letter('o'),
                        Key::Left,
                    ],
                    vec![Check::Raw("nihao".into())],
                ),
                stage(vec![Key::Backspace], vec![Check::Raw("hao".into())]),
            ],
        ),
    ]
}

/// 主词典质量探针:自然键序(无隔音符)→ 首选应为该词。仅在 `--dict` 时运行。
pub fn probes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("nihao", "你好"),
        ("xiexie", "谢谢"),
        ("zhongguo", "中国"),
        ("women", "我们"),
        ("xianzai", "现在"),
        ("shijian", "时间"),
        ("diannao", "电脑"),
        ("gongzuo", "工作"),
        ("shangban", "上班"),
        ("xuexi", "学习"),
        ("jintian", "今天"),
        ("tianqi", "天气"),
    ]
}
