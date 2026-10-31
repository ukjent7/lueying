# 候选评测与基准

为了客观衡量候选词的召回质量与排序准确率，掠影采用独立冻结的评测语料集进行端到端量化评测，词典只参与候选解码，绝不参与测试用例构造。

## 评测语料设计

基准语料固定存储于 `dicts/eval-shared-v1.tsv`，字段格式为：`category`（类别）、`source`（来源）、`pinyin`（输入拼音）、`expected`（期望文本）。

| 类别标识 | 用例数量 | 语料来源与特征 |
|---|---:|---|
| `daily` | 246 条 | 常见日常会话与通用文本，测试基础成句与常用搭配 |
| `frost` | 93 条 | 白霜经典长短句用例 |
| `hard` | 1,105 条 | 歧义与中立难例（C4 中文语料构造），用于压力测试分词与重排算法 |
| **合计** | **1,444 条** | 经过严格去重与合规清洗 |

### 评测归一化规则
为了消除无关异体字干扰并支持横向对比，评测对输出文本实施统一定则：
- `其它` → `其他`
- `她` / `它` → `他`
- `地` / `得` → `的`

## 指标体系与质量门限

评测系统考察以下关键指标：
- **Top-1 / Top-3 / Top-5 准确率**：期望文本出现在对应候选档位的比例。
- **MRR（平均倒数排名）**：衡量期望结果在候选列表中排位的紧凑程度。
- **延迟预算门限**：端到端击键响应 p95 ≤ 60ms；4M 实时同步重排 p95 ≤ 40ms；25M 异步重排 p95 ≤ 800ms。

### 基准表现参考

在标准生产配置下（启用语法搭配模型与神经重排模型）：

| 测试类别 | Top-1 | Top-3 | Top-5 | MRR | CI 准入门限（Top-1 / Top-5） |
|---|---:|---:|---:|---:|---|
| `daily` | ≥ 93.0% | ≥ 95.0% | ≥ 96.0% | ≥ 0.940 | 85.0% / 90.0% |
| `frost` | ≥ 72.0% | ≥ 81.0% | ≥ 87.0% | ≥ 0.770 | 65.0% / 80.0% |
| `hard` | ≥ 57.0% | ≥ 73.0% | ≥ 79.0% | ≥ 0.660 | 50.0% / 75.0% |

## 本地复现步骤

开发者可在开发环境中完整执行基准评估：

```sh
# 1. 抓取外部语料与模型
bash tools/fetch_frost.sh data/frost
bash tools/fetch_gram.sh data
bash tools/fetch_rerank.sh data/rerank

# 2. 构建物化二进制词典
cargo run -p xtask --release --locked -- dict --frost data/frost --out data/main.tsv --bin data/main.pyd

# 3. 运行全量端到端基准
cargo run -p lueying-e2e --release --locked -- \
  --dict data/main.tsv \
  --gram data/main.gram \
  --rerank-dir data/rerank \
  --only-shared
```

执行后将自动生成 `target/bench-shared.tsv`、`target/bench-shared.json` 及 Markdown 格式的完整汇总报告。在日常提交自测中，可附加 `--bench-fast` 参数仅跑抽样用例。

## 跨引擎对比机制

通过 `tools/bench_shared.py` 工具，可将同一份 `eval-shared-v1.tsv` 导出至其他输入引擎运行，并统一收敛结果以生成横向对比报告：

```sh
# 导出统一评测输入
python3 tools/bench_shared.py export --engine <目标引擎> --out /tmp/eval-input.tsv

# 运行对比生成并排表格
python3 tools/bench_shared.py compare target/bench-shared.tsv /tmp/engine1-result.tsv /tmp/engine2-result.tsv
```

### 评测原则

1. **同构对比**：跨引擎对比必须记录对应的词典规模、模型参数量与执行日期，避免脱离约束单独比较准确率。
2. **纯粹隔离**：临时测试报告与过程文件生成于 `target/` 或系统临时目录，严禁提交至版本仓库。
