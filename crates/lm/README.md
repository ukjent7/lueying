# chinese-ime-lm（内嵌副本）

来自 <https://github.com/metasequoiaime/chinese-ime-lm>，提交
`1c90f5ee1b76ecf18d5171ef78a4dc076a0bb0d8`（2026-11-04，release `rerank-v2`），目录 `reference/`，
对应 `reference` 0.2.1 的批式推理实现。许可 Apache-2.0，见同目录 `LICENSE`。

复制运行所需的 `src/lib.rs`、`src/weights.rs` 与上游单元测试文件。本地保留
`prefix_tokens` 剥掉词表外字符（上游 #91）、`best_where_margin`（8M 同步重排
要求明确领先），以及 `Variant`/`segment` 对按键模型的兼容改动。更新时整目录
替换，并把这些改动重新接回。

另外两个文件是本项目新增的：`src/ort.rs` 读取上游按键模型发布的 ORT 平面缓冲，
`src/keystroke.rs` 把 `lm_step.ort` 的三个文件加载成同一套 `SentenceModel`。
它们让 8M/25M 按键重排保持纯 Rust 推理，不引入 ONNX Runtime 原生依赖。

`src/math.rs` 的 `dispatched!` 也是本地改动：内核按 CPU 运行时选 AVX2，结果与基线逐位相同。
