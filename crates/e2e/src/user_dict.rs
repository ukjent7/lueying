//! 用户词库 E2E:先列失败方式,再逐条检查。
//!
//! 1. 选词学习只留在当前会话 → 共用一份词库的两个引擎必须互相看到。
//! 2. 导入重复行重复计分 → 同键同词合并,频次取最大。
//! 3. 导出/导入丢词或坏行连坐 → 往返后词频不变,坏行单独跳过。
//! 4. 上屏不经过学习 → 空格上屏后导出里应出现该键与该词。
//! 5. 用户词压不过词典 → 高频导入词必须排到同键词典词前面。
//! 6. 分段上屏学错键 → 上屏前缀词时只学它吃掉的音节。
//! 7. 长期学习让词库无限膨胀 → 超过上限后按频次/时间淘汰,文件大小有界。
//! 8. 多音节学习键存成原始串 → 新会话必须能用 `ni'hao` 这样的音节键复现提频。
//! 9. 组级适应只留在内存 → 导出/导入后模糊组计数必须还在。
//! 10. 学习时用简拼、查询时用全拼(或反过来)丢掉提频 → 两种输入形态必须互相识别,
//!     Shift+Delete 也要按词删掉所有形态的记录。
//! 11. 用户词库到 2 万条后每个音节组合线性扫全表 → 查询按音节数与首字母收窄,
//!     既不能漏掉该命中的用户词,也不能随词库增长拖慢首键。
//! 12. 上屏只学词频、跨词搭配只留在内存 → 前后字搭配必须随词库一起导出/导入,
//!     新会话能继续用同一份个人上下文。
//! 13. 上下文搭配只写文件、不参与排序 → 学过「最先」后,输入 `xian` 的首选
//!     必须从更高频的「现」切到「先」;标点切断相邻关系时必须退回「现」。
//! 14. 分段上屏后剩余候选在上下文写入前重算 → 下一段必须立刻看到刚上屏的
//!     左邻字,而不是等下一次按键才生效。
//! 15. 词级转移只留在内存 → 「你→好」这类相邻词对必须随词库导出/导入,
//!     新会话继续生效。
//! 16. 词级转移写了却不参与排序 → 上屏「最」后输入 `xian`,应偏向「先」。
//! 17. 同键跳过前面的候选不学习 → 再打同一键时它仍排在最前,用户每次都要多按一次。
//!     被越过的候选要记小负分,并在重新选中它时清除;快捷候选不参与。
//! 18. 跳过负分只留在内存 → 导出/导入后必须继续生效,Shift+Delete 也要能忘掉。
//! 19. 三词窗口只留在内存 → 「你」「好」「先」这类三词序列必须随词库导出/导入,
//!     新会话继续生效。
//! 20. 三词窗口写了却不参与排序 → 上屏「你」「好」后输入 `xian`,应偏向「先」;
//!     标点切断词序时必须退回「现」。
//! 21. 按应用偏好只留在内存 → Code.exe 里常选的词必须随词库导出/导入,
//!     应用名大小写不同也要归并成一份记录。
//! 22. 按应用偏好写了却不参与排序 → Code.exe 下输入 `xian` 应偏向「先」,
//!     换到 chrome.exe 或无宿主时仍以全局词频为准。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Event, SharedUser};

type Checks = Vec<(&'static str, bool, String)>;

fn push(checks: &mut Checks, name: &'static str, ok: bool, note: String) {
    checks.push((name, ok, note));
}

fn engine(dict: &Arc<Dictionary>, user: &SharedUser) -> Engine {
    Engine::new(Arc::clone(dict), Config::default())
        .with_user(Arc::clone(user))
        .with_modules(crate::builtins())
}

/// 翻到候选所在页并点选;用于重复学习同一个词。
fn pick_word(engine: &mut Engine, word: &str) -> bool {
    let Some(index) = engine.candidates().iter().position(|c| c == word) else {
        return false;
    };
    let page_size = engine.page_size().max(1);
    let page = index / page_size;
    while engine.page() < page {
        if !engine.page_by(1) {
            return false;
        }
    }
    engine
        .pick_in_page(index % page_size)
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text.as_str() == word))
}

#[path = "user_dict_context.rs"]
mod context;
#[path = "user_dict_reject.rs"]
mod reject;
#[path = "user_dict_wordbook.rs"]
mod wordbook;

pub fn run(report: &mut String) -> usize {
    let dict = Arc::new(Dictionary::builtin());
    let mut checks = Checks::new();
    // 导入的学习记录用当前时间,避免刚写入就被判为过期而衰减。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    context::commit_and_context(&dict, &mut checks, now);
    context::word_sequences(&dict, &mut checks, now);
    wordbook::app_and_shared(&dict, &mut checks, now);
    wordbook::pin_and_demote(&dict, &mut checks);
    wordbook::learning_keys(&dict, &mut checks);
    reject::rejection_and_scale(&dict, &mut checks, now);

    report.push_str("\n## 用户词库\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0usize;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}
