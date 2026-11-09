//! 候选类型与生成入口。解码见 [`crate::decoder`],读法见 [`crate::channel`]。

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_dict::syllables::Fuzzy;

use crate::decoder::{self, Charts};
use crate::user::UserDict;

/// 展示给上层的候选。
#[derive(Clone, Debug)]
pub struct Candidate {
    pub text: String,
    /// 候选文本的切词结果;单个前缀词就是它本身。
    pub words: Vec<String>,
    /// 吃掉的音节数。
    pub consume: usize,
    pub score: f64,
    /// 各音节的原始字节区间(回删与逐段上屏用)。
    pub spans: Vec<(usize, usize)>,
    /// 覆盖整个组合输入,而不是前缀词或联想补全。
    pub full: bool,
    /// 逐音节精确命中(含简拼),未经模糊扩写;用户词不算。
    pub trusted: bool,
    /// 两个及以上精确词典词拼成的整句。没有单词条频次,保护时另设门槛。
    pub sentence: bool,
    /// 模糊改动处数;0 表示逐音节精确命中。
    pub edits: usize,
    /// 词典词频(用户独有词为 0)。精确首位保护只锁有 corpus 依据的常用词。
    pub freq: u64,
    /// 同一按键下用户选过该词的次数。
    pub user_freq: u32,
    /// 用户词库在该键下精确学过这个词(全拼与简拼互认),可作为首位覆盖的依据。
    pub learned: bool,
    /// 命中的模糊组位掩码(0 表示精确)。
    pub groups: u8,
    /// 模糊候选的注音提示(如 `shi`);精确候选为空。
    pub hint: Option<String>,
    /// 纠正候选的学习键(原串与纠正串不同,不能从 `spans` 反推)。
    pub learn_key: Option<String>,
    /// 手误纠正产生的候选;界面上用灰字标出原拼音,学习按纠正后的键。
    pub correction: bool,
    /// 这种读法的信道代价(nat):模糊、手误、补全越多越大。重排模型只看文字,
    /// 比较候选时要把它算回去。
    pub channel: f64,
    /// `score` 里来自用户学习的部分:选词次数、上下文、词序、应用偏好,减去跳过负分。
    /// 重排给引擎分的权重很小,这一份要单独按完整权重算回去。
    pub personal: f64,
    /// 快捷输入(日期/时间/数字金额/符号):不写词库,也不参与词级转移。
    pub quick: bool,
    /// 用户在同键下跳过过该候选:只小幅降权,精确首位保护也会让位。
    pub rejected: bool,
    /// 显示文本是繁体时的简体规范形式。学习与左邻上下文用它,避免简繁分裂词库。
    pub learn_text: Option<String>,
    /// 表情、英文直通等固定候选:不学习、不造词,也不参与模型重排。
    pub no_learn: bool,
    /// 末音节补全候选:吃掉整串输入,学习键是补全后的音节串。
    pub completion: bool,
    /// 用户在当前键下置顶的词:直接放首位,重排与快捷项都让位。
    pub pinned: bool,
    /// 动作候选要执行的完整命令 id;普通候选为空。动作候选不上屏文字、
    /// 不学习,选中即执行命令。
    pub action: Option<String>,
}

impl Candidate {
    /// 功能模块添加“整串上屏”候选的最小构造:一个区间覆盖整个原始输入,
    /// 上屏后组合结束。`quick` 保持 false,由模块按需改写。
    pub fn whole(raw_len: usize, text: impl Into<String>, score: f64) -> Self {
        let text = text.into();
        Self {
            words: vec![text.clone()],
            text,
            consume: 1,
            score,
            spans: vec![(0, raw_len)],
            full: true,
            trusted: false,
            sentence: false,
            edits: 0,
            freq: 0,
            user_freq: 0,
            learned: false,
            groups: 0,
            hint: None,
            learn_key: None,
            correction: false,
            channel: 0.0,
            personal: 0.0,
            quick: false,
            rejected: false,
            learn_text: None,
            no_learn: false,
            completion: false,
            pinned: false,
            action: None,
        }
    }

    /// 覆盖整个输入的固定快捷候选:不学习、不参与重排,顺序由插入位置决定。
    pub fn quick(raw_len: usize, text: impl Into<String>, score: f64) -> Self {
        let mut cand = Self::whole(raw_len, text, score);
        cand.words.clear();
        cand.quick = true;
        cand
    }
}

/// 一次构建的产物。
pub struct Built {
    pub cands: Vec<Candidate>,
}

/// 一次候选构建的全部输入。
pub(crate) struct BuildArgs<'a> {
    pub dict: &'a Dictionary,
    pub user: &'a UserDict,
    pub fuzzy: &'a Fuzzy,
    pub gram: Option<&'a Gram>,
    /// 上一次解码的前缀图,按键之间复用。
    pub charts: &'a mut Charts,
    pub context: &'a str,
    pub host: &'a str,
    pub previous2: Option<&'a str>,
    pub previous: Option<&'a str>,
    pub raw: &'a str,
    /// 当前串处于快捷模块的触发会话;触发键要秒出,不加手误读法。
    pub quick_trigger: bool,
}

/// 候选生成入口。
pub(crate) fn build(args: BuildArgs<'_>) -> Built {
    decoder::decode(args)
}
