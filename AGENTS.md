# 掠影开发备忘

## 基准尺子

- 3600 句产品链路尺子（learn-sim 副作用臂,逐句名次落盘
  `target/learn-sim-side.tsv`）:

  ```bash
  cargo run --release -p lueying-bench -- learn-sim \
    --dict data/main.pyd --gram data/collocation.bin \
    --rerank-dir data/rerank --full --parts side
  ```

  **`--gram` 不能省**:搭配表参与解码排序,省掉后候选池与池序都不同
  (与 `pool-dump`、历史基线不可比),数字会偏 ~1pp。

- 快速门禁(与 CI `bench-accuracy.yml` 同口径,约 21s):

  ```bash
  python3 tools/bench_accuracy.py quick \
    --bin target/release/lueying-bench --report target/accuracy-quick.md
  ```

- 候选池 + 双模型逐字分离线转储(离线策略模拟用):

  ```bash
  cargo run --release -p lueying-bench -- pool-dump \
    --dict data/main.pyd --gram data/collocation.bin \
    --rerank-dir data/rerank --out target/pool-dump.jsonl
  ```

  配套离线模拟器见 `/workspace/exp/simulate.py`(逐句复刻产品链路)。
