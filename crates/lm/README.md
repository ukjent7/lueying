# chinese-ime-lm（内嵌副本）

来自 <https://github.com/metasequoiaime/chinese-ime-lm>，提交
`8a22e8508138270d2508548b7c1de5fc549120ca`（2026-11-02），目录 `reference/`，
对应 `reference` 0.2.1 的批式推理实现。许可 Apache-2.0，见同目录 `LICENSE`。

复制运行所需的 `src/lib.rs`、`src/weights.rs` 与上游单元测试文件。本地保留两处
改动：`prefix_tokens` 剥掉词表外字符（上游 #91），以及 `best_where_margin`
（4M 同步重排要求明确领先）。更新时整目录替换，并把这两处改动重新接回。
