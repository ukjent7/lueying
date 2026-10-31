# 候选基准

候选质量用仓库外冻结语料衡量，词典只生成候选、不参与造题。共享集固定在 `dicts/eval-shared-v1.tsv`，四列：`category`、`source`、`pinyin`、`expected`。

| 类别 | 条数 | 来源 |
|---|---:|---|
| `daily` | 246 | 水杉 `sentences-v1` + 清风 `grammar_sentence_eval` |
| `frost` | 93 | 白霜 `script.lua` |
| `hard` | 1,105 | 水杉中立难例，C4 中文 |
| 合计 | 1,444 | 去重后 |

判定归一化：其它→其他，她/它→他，地/得→的。同一份语料、同一套归一化才能横向比较。

## 掠影当前结果

2026-10-31 的 `quality` artifact，默认配置（模糊自动、语法模型开启、4M/25M 重排加载）：

| 类别 | top-1 | top-3 | top-5 | MRR | 门限 |
|---|---:|---:|---:|---:|---|
| `daily` | 93.1% | 95.5% | 96.3% | 0.943 | 85 / 90 |
| `frost` | 72.0% | 81.7% | 87.1% | 0.776 | 65 / 80 |
| `hard` | 57.7% | 73.1% | 79.0% | 0.661 | 50 / 75 |

词级外部词表（2,998 条微软经典示例）当前 top-1 79.2%、top-5 95.2%。难例按构造全是引擎原序会错的句子，绝对值不能和日常集直接相减。

复现：

```sh
bash tools/fetch_frost.sh data/frost
bash tools/fetch_gram.sh data
bash tools/fetch_rerank.sh data/rerank
cargo run -p xtask --release --locked -- dict --frost data/frost --out data/main.tsv --bin data/main.pyd
cargo run -p lueying-e2e --release --locked -- --dict data/main.tsv --gram data/main.gram --rerank-dir data/rerank --only-shared
```

输出 `target/bench-shared.tsv`、`target/bench-shared.json` 与 `target/e2e-report.md`。`--bench-fast` 走等距子集，适合每次 push；`--bench` 与 `--only-shared` 覆盖全量。

## 跨引擎对比

`tools/bench_shared.py` 负责导出输入、导入各家结果、生成并排 Markdown。参考项目放在 `work/`，不参与本仓库 CI；对端需要各自的数据与构建环境。

青简：

```sh
python3 tools/bench_shared.py export --engine qingjian --out /tmp/qj.tsv
(cd work/qingjian-main && cargo run --release -p qingjian-cli -- --eval-text /tmp/qj.tsv --eval-details /tmp/qj.jsonl)
python3 tools/bench_shared.py import-qingjian --details /tmp/qj.jsonl --out /tmp/qj-result.tsv
```

清风：

```sh
python3 tools/bench_shared.py export --engine windinput --out /tmp/wind.tsv
(cd work/WindInput-main/wind_input && WIND_SENTENCE_EVAL=/tmp/wind.tsv cargo test --release -p wind-engine --test grammar_sentence_eval sentence_eval_file -- --ignored --nocapture > /tmp/wind.txt)
python3 tools/bench_shared.py import-windinput --stdout /tmp/wind.txt --out /tmp/wind-result.tsv
```

水杉：

```sh
python3 tools/bench_shared.py export --engine msime --out /tmp/msime.tsv
(cd work/msime-develop && cargo run --release -p msime-input-runtime --example convert_eval -- \
  --resources <已校验的资源目录> --set /tmp/msime.tsv --dump /tmp/msime.jsonl)
python3 tools/bench_shared.py import-msime --dump /tmp/msime.jsonl --out /tmp/msime-result.tsv
```

并排：

```sh
python3 tools/bench_shared.py compare \
  target/bench-shared.tsv /tmp/qj-result.tsv /tmp/wind-result.tsv /tmp/msime-result.tsv
```

水杉的 `convert_eval` 需要按其资源锁校验词库目录；`--dump` 是逐条候选，`--report` 只有汇总。导出器把 `syllables` 填成期望文本的汉字数，全键盘评测不读这一列，九键评测要另按水杉的九键集跑。

## 使用边界

- 对端版本、词典与模型版本要和结果一起记录；跨版本的数字不可直接比较。
- 本地临时结果放 `target/` 或 `/tmp/`，不要入库；要写进文档时同时写日期、引擎版本与配置。
- 准确率之外还要看延迟。掠影的按键链路门限是 p95 ≤ 60 ms；4M 决策 p95 ≤ 40 ms、25M 停顿 p95 ≤ 800 ms，测量脚本在 `crates/e2e/src/rerank.rs` 与 `crates/server/src/ui_skia_bench.rs`。