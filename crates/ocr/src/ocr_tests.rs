use super::*;

#[test]
fn open_reports_missing_model() {
    let dir = std::env::temp_dir().join(format!(
        "lueying-ocr-missing-{}",
        std::process::id()
    ));
    let Err(error) = PpOcr::open(&dir) else {
        panic!("缺少模型时不应加载成功");
    };
    assert!(error.contains("det.onnx"), "{error}");
}

/// 真模型回归;CI 用 `tools/fetch_ppocr.sh` 拉模型后以 `--ignored` 跑。
#[test]
#[ignore = "需要 PP-OCRv6 模型:LUEYING_PPOCR_DIR"]
fn recognizes_committed_sample() {
    let Some(dir) = std::env::var_os("LUEYING_PPOCR_DIR") else {
        return;
    };
    let model = PpOcr::open(Path::new(&dir)).expect("加载模型");
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/sample.bmp");
    let image = load_bmp(&sample).expect("读样例");
    let lines = model.recognize(&image).expect("识别");
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("LUEYING"), "{text}");
    assert!(text.contains("2026"), "{text}");
}
