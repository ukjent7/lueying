# 评测

## 语料

`dicts/eval-shared-v1.tsv` 是冻结的端到端语料，共 1544 条，分四类：

| 类别 | 条数 | 说明 |
|---|---:|---|
| `daily` | 246 | 日常会话与通用文本 |
| `hard` | 1105 | 歧义与中立的难例，压力测试分词与重排 |
| `context` | 100 | 带上文的用例，检验上下文重排 |
| `frost` | 93 | 早期长句样本，类别名沿用旧名称 |

用例不由词典构造。词典只参与候选生成。比对时对期望文本与候选统一归一化：`其它`→`其他`，`她`、`它`→`他`，`地`、`得`→`的`。

## 门限

CI 对每类的 top-1 与 top-5 设门限（`tools/bench_shared.py` 中的 `GATES`）：

| 类别 | top-1 | top-5 |
|---|---:|---:|
| `daily` | 85% | 90% |
| `hard` | 50% | 75% |
| `frost` | 65% | 80% |
| `context` | 60% | 80% |

门限是底线，不是目标。

## 复现

```sh
bash tools/fetch_dict.sh data
bash tools/fetch_rerank.sh data/rerank
cargo run -p lueying-bench --release --locked -- \
  --dict data/main.pyd --gram data/collocation.bin \
  --rerank-dir data/rerank --only-shared
python3 tools/bench_shared.py summarize target/bench --print
```

逐条结果写入 `target/bench/<语料>.<分片>.tsv`。`--corpus <文件>` 换用其他语料，`--shard <序号>/<总数>` 只跑其中一片。`summarize --gate` 在不达门限时返回非零退出码。

`summary.json` 按语料、类别与链路（`engine`、`8m`、`settled`）给出 top-1/3/5、MRR、Wilson 区间、耗时、候选池覆盖，以及逐段选词打出整句的按键成本（选词次数与翻页数）。`macro` 是各类等权平均。

## V2 开发集与测试集

- `dicts/eval-dev-v4.tsv`：调参只看这一份(3600 条)。由 `tools/make_dev_v4.py` 从 V2 开发集和 V3 分段语料派生:词级每类最多 300 条,分段(1–3 字,带真实上文)全部保留,整句只留全拼与混打各 300 条做回归。去掉整句声母和整句手误/模糊音。
- `dicts/eval-v2-test.tsv`：只在发版前看,`dict-bench` 工作流勾选 `test` 时才运行。
- `dicts/eval-v2-dev.tsv`：派生 v4 的原始来源,不再直接用于调参。

V2 来自 chinese-ime-lm 的 `sentences-ctx-v1`,文本不在词典与重排模型的训练语料中。
开发集 v4 的选取以哈希决定,换开发集会让基线失效,必须用新开发集重跑基线后再比较。

## 压力语料 v3

`dicts/eval-v3-{dev,test}.tsv` 由 `tools/make_eval_v3.py` 从 V2 的全拼整句派生,每类最多 200 条。开发集与测试集分别派生。

用户的真实打法是一次上屏 1–3 个字,每段的上文是已上屏的正确文字,所以主要派生是分段:

| 类别前缀 | 派生方式 |
|---|---|
| `chunk-full-` | 按 1–3 字切段,每段全拼,上文是前面已上屏的正确文字 |
| `chunk-fuzzy-` | 同上,段内随机换 1 个音节为模糊音(zh/z、ch/c、sh/s、l/n、f/h、an/ang、en/eng、in/ing、ian/iang、uan/uang) |
| `chunk-typo-` | 同上,段内 1–2 处手误:相邻键替换、漏键、多键、相邻字母调换 |
| `fuzzy-clause-`、`typo-clause-` | 整句级的模糊音与手误,作为跨段对照 |

派生只改输入串,期望答案取自 V2 原句。句子级两半零重叠;短词在两半都出现属于 V2 原有结构。

## 用户自测语料

`dicts/eval-user-v{1,2,3}.tsv` 是用户自己打的整句(`user-sentence`,共 60 句),键入串原样保留缩写、模糊音、漏打和手误。标点不计入键入串和目标句。`dicts/eval-user-v{1,2,3}-intervals.tsv` 记录他自己的空格(上屏间隔),后续分段测试以它为准,不由维护者切段。这份语料只用于评测,不用于训练。句子少,单句结果噪声大,看趋势不看单句。

`dicts/eval-user-v4.tsv` 是整句纠错专项(50 句):用户按预先出好的目标句自然打字,错误原样采集,分词空格已剥除(引擎收不到空格)。采集时先只发目标句、收齐键入串再建库,保证期望答案先于键入串存在。

## 核心指标与基线门限

`summarize` 的 `core` 节是核心类别的宏平均:全拼、词级简拼与混打、整句混打、`chunk-`、`fuzzy-`、`typo-`、`user-`。纯声母整句(`clause-initials-`)只记录,不计入。

`summarize --baseline <上一次的 summary.json>` 检查干净全拼用例(`clause-full-`、`word-full-`)在产品链路 `settled` 上的加权首选准确率,不能低于基线超过 `--tolerance`(默认 0 个百分点)。

## 私有语料

`dicts/eval-private.tar.gpg` 是加密的真实评论区文本，只用于评测，不参与训练。它用 GnuPG 以 AES-256 对称加密，密钥是仓库的 `EVAL_KEY`。没有密钥时 CI 跳过这份语料。公开产物只包含汇总数字。

## 跨引擎对比

```sh
python3 tools/bench_shared.py diff <结果A> <结果B> --samples 5
```

输出各类的增量、McNemar 精确检验的 p 值，以及被改好与改坏的样本。n = 60 时置信区间半宽约 ±13 个百分点，比较时看差值与不一致的样本对，不要只看单次汇总。
