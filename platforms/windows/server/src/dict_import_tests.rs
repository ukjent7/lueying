use super::*;

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[test]
fn decodes_utf8_with_and_without_bom() {
    assert_eq!(decode("你好\tnihao\n".as_bytes()), "你好\tnihao\n");
    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice("你好".as_bytes());
    assert_eq!(decode(&with_bom), "你好");
}

#[test]
fn decodes_utf16_with_bom() {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in "你好".encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    assert_eq!(decode(&bytes), "你好");
}

#[test]
fn decodes_gb18030_when_not_utf8() {
    let (encoded, _, _) = encoding_rs::GB18030.encode("词库");
    assert_eq!(decode(&encoded), "词库");
}

#[test]
fn parses_standard_tsv_dictionary() {
    let text = "你好\tni hao\t1000\n中国\tshi'zhong\t20000\n";
    let report = parse(text);
    assert_eq!(report.imported, 2);
    assert_eq!(report.skipped, 0);
    let rows = lines(&report.text);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].starts_with("ni'hao\t你好\t"), "{rows:?}");
    assert!(rows[1].starts_with("shi'zhong\t中国\t"), "{rows:?}");
}

#[test]
fn parses_continuous_pinyin_without_separator() {
    let report = parse("你好\tnihao\t1000\n");
    assert_eq!(report.imported, 1);
    assert!(report.text.starts_with("ni'hao\t你好\t"), "{}", report.text);
}

#[test]
fn weight_becomes_log_scaled_frequency() {
    // 权重 1000 → 词频 10,导入的词压过普通候选又不至于清场。
    let report = parse("你好\tni hao\t1000\n");
    let row = report.text.lines().next().unwrap();
    let cols: Vec<&str> = row.split('\t').collect();
    let freq: u32 = cols[2].parse().unwrap();
    assert_eq!(freq, 10);
}

#[test]
fn keeps_native_export_lines_untouched() {
    let text = "ni'hao\t你好\t3\t1700000000\n";
    let report = parse(text);
    assert_eq!(report.imported, 1);
    assert_eq!(report.text, text);
}

#[test]
fn keeps_native_abbreviated_key() {
    // 简拼学出来的键不是音节,原生导出照样并得回去。
    let text = "n'h\t你好\t3\t1700000000\n";
    let report = parse(text);
    assert_eq!(report.imported, 1);
    assert_eq!(report.text, text);
}

#[test]
fn keeps_engine_stat_lines() {
    // 导出文件里的统计行要原样回去,不然模糊组与置顶就丢了。
    let text = "@group\tzcs\t5:1700000000\n@pin\tni'hao\t你好\n";
    let report = parse(text);
    assert_eq!(report.imported, 0);
    assert_eq!(report.text, text);
}

#[test]
fn standard_row_with_extra_column_is_not_native() {
    let report = parse("你好\tni hao\t1000\t2020\n");
    assert_eq!(report.imported, 1);
    assert!(report.text.starts_with("ni'hao\t你好\t"), "{}", report.text);
}

#[test]
fn skips_rows_whose_syllable_count_misses() {
    let report = parse("你好\tni\t1000\n");
    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped, 1);
}

#[test]
fn reads_hanzi_only_list_from_main_dictionary() {
    let (dict, bad) = Dictionary::from_text("重庆\t100\tchong'qing\n北京\t200\tbei'jing\n");
    assert_eq!(bad, 0);
    let mut report = parse("重庆\n不认识\n");
    assert!(report.needs_dict());
    report.resolve(Some(&dict));
    assert_eq!(report.imported, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.no_pinyin, 1);
    assert!(report.text.starts_with("chong'qing\t重庆\t"), "{}", report.text);
}

#[test]
fn hanzi_only_list_without_dictionary_is_skipped() {
    let mut report = parse("重庆\n");
    report.resolve(None);
    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.no_pinyin, 1);
}

#[test]
fn normalizes_pinyin_aliases() {
    assert_eq!(key_of("lue").as_deref(), Some("lve"));
    assert_eq!(key_of("nue").as_deref(), Some("nve"));
    assert_eq!(key_of("ni3 hao3").as_deref(), Some("ni'hao"));
    assert_eq!(key_of("ni hao").as_deref(), Some("ni'hao"));
    assert_eq!(key_of("nihao").as_deref(), Some("ni'hao"));
    assert_eq!(key_of("中文").as_deref(), None);
    assert_eq!(key_of("").as_deref(), None);
}

#[test]
fn splits_into_line_aligned_chunks() {
    let text = "a\tb\t1\t2\n".repeat(10);
    let chunks = split(&text, 30);
    assert_eq!(chunks.len(), 3);
    assert!(chunks.iter().all(|chunk| chunk.ends_with('\n')));
    let joined: String = chunks.concat();
    assert_eq!(joined, text);
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let report = parse("# 词表头\n\n你好\tni hao\t1000\n");
    assert_eq!(report.imported, 1);
    assert_eq!(report.skipped, 0);
}
