//! 导入的特征测试:锁定 `import` 对每种行、坏行与同键合并的现有行为。
//!
//! 断言都落在 `export` 的文本上:导出按内容排序,且不依赖当前时间。

use lueying_dict::syllables::FUZZY_GROUPS;

use crate::UserDict;

const GROUP: &str = FUZZY_GROUPS[0];

/// 导入一段文本再导出,得到规范文本。
fn round_trip(text: &str) -> String {
    let mut user = UserDict::new();
    user.import(text);
    user.export()
}

#[test]
fn word_lines_export_sorted_and_merge_by_maximum() {
    let text = "ni\t你\t5\t100\nni\t你\t3\t200\nhao\t好\t2\t50\n";
    assert_eq!(round_trip(text), "hao\t好\t2\t50\nni\t你\t5\t200\n");
}

#[test]
fn every_control_kind_survives_a_round_trip() {
    let lines = [
        format!("@group\t{GROUP}\t3:100"),
        "@context\t你\t好\t2\t100".to_string(),
        "@wordctx\t_\t好\t1".to_string(),
        "@wordctx\t你\t好\t2".to_string(),
        "@transition\t你\t好\t1\t10".to_string(),
        "@trigram\t我\t们\t好\t1\t10".to_string(),
        "@app\tnotepad.exe\t你\t1\t10".to_string(),
        "@reject\tni\t你\t1\t10".to_string(),
        "@pin\tni\t你".to_string(),
    ];
    let text = lines.join("\n") + "\n";
    assert_eq!(round_trip(&text), text);
}

#[test]
fn word_context_counts_follow_the_left_character() {
    let mut user = UserDict::new();
    user.learn_word_context("去", "店", false);
    user.learn_word_context("去", "店", false);
    assert_eq!(user.word_context_count('去', "店"), 2);
    assert_eq!(user.word_context_count('在', "店"), 0);
    assert_eq!(user.export(), "@wordctx\t去\t店\t2\n");
}

#[test]
fn repeated_statistics_keep_the_larger_count_and_latest_time() {
    let text = "@context\t你\t好\t2\t100\n@context\t你\t好\t5\t50\n";
    assert_eq!(round_trip(text), "@context\t你\t好\t5\t100\n");
}

#[test]
fn app_host_names_are_trimmed_and_lowercased() {
    let text = "@app\t  Notepad.EXE \t你\t1\t10\n";
    assert_eq!(round_trip(text), "@app\tnotepad.exe\t你\t1\t10\n");
}

#[test]
fn malformed_lines_are_skipped() {
    let text = [
        "ni\t你\tx\t1",
        "ni\t你\t1",
        "ni\t你\t1\t2\textra",
        "\t你\t1\t2",
        "@group\tnot-a-group\t3:100",
        "@group\tno-colon",
        "@context\t你\t好\t0\t100",
        "@context\tab\t好\t2\t100",
        "@transition\t\t好\t1\t1",
        "@trigram\t我\t\t好\t1\t1",
        "@app\tnotepad.exe\t你\t1",
        "@reject\tni\t你\t1\t2\textra",
        "@pin\tni\t",
        "@pin\tni\t你\textra",
        "",
    ]
    .join("\n");
    assert_eq!(round_trip(&text), "");
}

#[test]
fn unknown_at_prefixed_key_is_read_as_a_word() {
    assert_eq!(round_trip("@foo\tbar\t1\t2\n"), "@foo\tbar\t1\t2\n");
}

#[test]
fn export_is_a_fixed_point_of_import() {
    let text = "ni\t你\t5\t100\n@context\t你\t好\t2\t100\n@pin\tni\t你\n";
    let once = round_trip(text);
    assert_eq!(round_trip(&once), once);
}
