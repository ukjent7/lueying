//! E2E 场景规格:每个场景列出「它可能怎么坏」。分阶段推进,阶段末逐条断言。
//! 全部基于内置迷你词库;主词典质量与性能探针见 [`probes`]。

use lueying_engine::{Key, Mode};

pub struct Scenario {
    pub name: &'static str,
    /// 该场景显式关闭自动模糊音(`config.ini` 的 `fuzzy=off`)。
    pub fuzzy_off: bool,
    /// 场景开始时打开全角模式。
    pub fullwidth: bool,
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
    /// 组合光标在原始串里的字节位置。
    Cursor(usize),
    NoComp,
    /// 模式序列(含初始 Chinese)。
    ModeSeq(Vec<Mode>),
    /// 至少一个直通事件。
    Passthrough,
    /// 指定候选带注音提示。
    Hint {
        text: String,
        hint: String,
    },
    /// 用户词库里已有这个「音节键 → 词」。
    UserHas {
        key: &'static str,
        word: &'static str,
    },
    /// 某模糊组的有效学习次数至少为 `min`。
    GroupAtLeast {
        group: &'static str,
        min: u32,
    },
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

fn s(name: &'static str, fuzzy_off: bool, stages: Vec<Stage>) -> Scenario {
    sf(name, fuzzy_off, false, stages)
}

fn sf(name: &'static str, fuzzy_off: bool, fullwidth: bool, stages: Vec<Stage>) -> Scenario {
    Scenario {
        name,
        fuzzy_off,
        fullwidth,
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
            "ü 写作 ue:lue/nue 兼容",
            false,
            vec![
                stage(l("lue"), vec![Check::Top("略".into())]),
                stage(
                    vec![Key::Space],
                    vec![
                        commits(&["略"]),
                        Check::UserHas {
                            key: "lve",
                            word: "略",
                        },
                        Check::NoComp,
                    ],
                ),
                stage(l("nue"), vec![Check::Top("虐".into())]),
                stage(vec![Key::Space], vec![commits(&["虐"]), Check::NoComp]),
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
            "Backspace逐字母回删",
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
                    vec![Check::Raw("niha".into())],
                ),
                stage(
                    vec![Key::Backspace, Key::Backspace],
                    vec![Check::Raw("ni".into())],
                ),
                stage(vec![Key::Space], vec![commits(&["你"]), Check::NoComp]),
            ],
        ),
        s(
            "Ctrl+Backspace/方向键按音节编辑",
            false,
            vec![
                stage(
                    l("nihaoma"),
                    vec![Check::Raw("nihaoma".into()), Check::Cursor(7)],
                ),
                stage(
                    vec![Key::SegmentBackspace],
                    vec![Check::Raw("nihao".into()), Check::Cursor(5)],
                ),
                stage(vec![Key::SegmentLeft], vec![Check::Cursor(2)]),
                stage(vec![Key::SegmentLeft], vec![Check::Cursor(0)]),
                stage(vec![Key::SegmentRight], vec![Check::Cursor(2)]),
                stage(
                    vec![Key::SegmentDelete],
                    vec![Check::Raw("ni".into()), Check::Cursor(2)],
                ),
                stage(vec![Key::SegmentDelete], vec![Check::Raw("ni".into())]),
                stage(vec![Key::Esc], vec![Check::NoComp]),
                stage(
                    vec![
                        Key::Letter('x'),
                        Key::Letter('i'),
                        Key::Punct('\''),
                        Key::Letter('a'),
                        Key::Letter('n'),
                    ],
                    vec![Check::Raw("xi'an".into())],
                ),
                stage(
                    vec![Key::SegmentBackspace],
                    vec![Check::Raw("xi".into()), Check::Cursor(2)],
                ),
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
                    Key::Punct('['),
                    Key::Punct(']'),
                    Key::Punct('{'),
                    Key::Punct('}'),
                    Key::Punct('$'),
                    Key::Punct('^'),
                    Key::Punct('_'),
                    Key::Punct('\\'),
                    Key::Punct('`'),
                    Key::Punct('~'),
                    Key::Punct('|'),
                    Key::Punct('"'),
                    Key::Punct('"'),
                    Key::Punct('\''),
                    Key::Punct('\''),
                ],
                vec![commits(&[
                    "，", "。", "！", "？", "；", "：", "（", "）", "《", "》", "【", "】", "「",
                    "」", "￥", "……", "——", "、", "·", "～", "｜", "“", "”", "‘", "’",
                ])],
            )],
        ),
        sf(
            "全角模式:英文/数字/空格/标点",
            false,
            true,
            vec![
                stage(
                    vec![Key::Shift],
                    vec![Check::ModeSeq(vec![Mode::Chinese, Mode::English])],
                ),
                stage(vec![Key::Letter('a')], vec![commits(&["ａ"])]),
                stage(vec![Key::Digit(3)], vec![commits(&["３"])]),
                stage(vec![Key::Space], vec![commits(&["　"])]),
                stage(vec![Key::Punct('.')], vec![commits(&["．"])]),
                stage(vec![Key::Punct(';')], vec![commits(&["；"])]),
                stage(vec![Key::Punct('/')], vec![commits(&["／"])]),
                stage(
                    vec![Key::Shift],
                    vec![Check::ModeSeq(vec![
                        Mode::Chinese,
                        Mode::English,
                        Mode::Chinese,
                    ])],
                ),
                stage(vec![Key::Digit(1)], vec![commits(&["１"])]),
                // 全角数字后仍是小数点形态,不写成中文句号。
                stage(vec![Key::Punct('.')], vec![commits(&["．"])]),
                stage(vec![Key::Letter('A')], vec![commits(&["Ａ"])]),
                stage(vec![Key::Space], vec![commits(&["　"])]),
                // 中文标点优先于全角 ASCII:句号仍是「。」。
                stage(vec![Key::Punct('.')], vec![commits(&["。"])]),
            ],
        ),
        s(
            "数字后的句号保持半角",
            false,
            vec![
                stage(
                    vec![Key::Digit(3)],
                    vec![Check::Passthrough, Check::NoCommits],
                ),
                // 数字后的第一个句点按小数点直通;之后的句点恢复中文句号。
                stage(
                    vec![Key::Punct('.')],
                    vec![Check::Passthrough, Check::NoCommits],
                ),
                stage(vec![Key::Punct('.')], vec![commits(&["。"])]),
            ],
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
                // 首页五条全是整句,最高分前缀词「今天」在第 6 位(第二页首条)。
                stage(vec![Key::PageDown], vec![]),
                stage(
                    vec![Key::Digit(1)],
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
            "Home+Delete逐字母前删",
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
                vec![Check::Raw("intian".into())],
            )],
        ),
        s(
            "默认自动模糊:si首选四且含是",
            false,
            vec![stage(
                l("si"),
                vec![Check::Top("四".into()), Check::Contains("是".into())],
            )],
        ),
        s(
            "默认自动模糊:shi首选是且含四",
            false,
            vec![stage(
                l("shi"),
                vec![Check::Top("是".into()), Check::Contains("四".into())],
            )],
        ),
        s(
            "显式关闭模糊:si不含是",
            true,
            vec![stage(
                l("si"),
                vec![Check::Top("四".into()), Check::NotContains("是".into())],
            )],
        ),
        s(
            "学习覆盖:si选是两次后首选是",
            false,
            vec![
                stage(
                    l("si"),
                    vec![Check::Top("四".into()), Check::Contains("是".into())],
                ),
                stage(vec![Key::Digit(2)], vec![commits(&["是"])]),
                stage(
                    l("si"),
                    vec![Check::Top("四".into()), Check::Contains("是".into())],
                ),
                stage(vec![Key::Digit(2)], vec![commits(&["是"])]),
                stage(l("si"), vec![Check::Top("是".into())]),
            ],
        ),
        s(
            "简拼首字母模糊:lh→你好",
            false,
            vec![
                stage(l("lh"), vec![Check::Top("你好".into())]),
                stage(vec![Key::Space], vec![commits(&["你好"])]),
            ],
        ),
        s(
            "整句模糊:zongguo→中国",
            false,
            vec![
                stage(l("zongguo"), vec![Check::Top("中国".into())]),
                stage(vec![Key::Space], vec![commits(&["中国"])]),
            ],
        ),
        s(
            "模糊候选带注音",
            false,
            vec![stage(
                l("si"),
                vec![
                    Check::Top("四".into()),
                    Check::Hint {
                        text: "是".into(),
                        hint: "shi".into(),
                    },
                ],
            )],
        ),
        s(
            "相邻键纠正:nihap→你好",
            false,
            vec![
                stage(
                    l("nihap"),
                    vec![
                        Check::Top("你好".into()),
                        Check::Hint {
                            text: "你好".into(),
                            hint: "纠正 ni hao".into(),
                        },
                    ],
                ),
                stage(
                    vec![Key::Space],
                    vec![
                        commits(&["你好"]),
                        Check::UserHas {
                            key: "ni'hao",
                            word: "你好",
                        },
                    ],
                ),
            ],
        ),
        s(
            "相邻字母调换纠正:hoa→好",
            false,
            vec![
                stage(
                    l("hoa"),
                    vec![
                        Check::Top("好".into()),
                        Check::Hint {
                            text: "好".into(),
                            hint: "纠正 hao".into(),
                        },
                    ],
                ),
                stage(
                    vec![Key::Space],
                    vec![
                        commits(&["好"]),
                        Check::UserHas {
                            key: "hao",
                            word: "好",
                        },
                    ],
                ),
            ],
        ),
        s(
            "组级学习:同一模糊组连续选择计数",
            false,
            vec![
                stage(l("zongguo"), vec![Check::Top("中国".into())]),
                stage(vec![Key::Space], vec![commits(&["中国"])]),
                stage(l("zongguo"), vec![Check::Top("中国".into())]),
                stage(vec![Key::Space], vec![commits(&["中国"])]),
                stage(l("zongguo"), vec![Check::Top("中国".into())]),
                stage(vec![Key::Space], vec![commits(&["中国"])]),
                stage(
                    l("zongguo"),
                    vec![Check::GroupAtLeast {
                        group: "zcs",
                        min: 3,
                    }],
                ),
            ],
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
            "长输入不截断",
            false,
            vec![stage(
                l("gaodewohaihuaiyishibushizijidacuoguanjiancile"),
                vec![Check::Raw(
                    "gaodewohaihuaiyishibushizijidacuoguanjiancile".into(),
                )],
            )],
        ),
        s(
            "方向键逐字母移动+回删",
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
                stage(vec![Key::Backspace], vec![Check::Raw("niho".into())]),
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
        // 音节表曾漏掉 di/neng/yue/deng/rui,这几条锁死补全后的行为。
        ("di", "地"),
        ("neng", "能"),
        ("yue", "月"),
        ("deng", "等"),
        ("rui", "瑞"),
    ]
}
