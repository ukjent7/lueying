# 语法模型：万象 LTS + librime-octagram 打分

## 选型

使用 [万象拼音](https://github.com/amzxyz/rime-wanxiang) 的
`wanxiang-lts-zh-hans.gram`（`amzxyz/rime-wanxiang-data` 的 `LTS` release，
2026-10-19，约 380 MiB）。它由 48 GB 级语料训练，键空间约 9962 万 unit。

替换掉的旧模型是 `lotem/rime-octagram-data` 的
`zh-hans-t-essay-bgw-compact.gram`（约 39 MiB，1023 万 unit）。

## 打分

`crates/dict/src/gram.rs` 复刻 `lotem/librime-octagram` 的 `Query` 语义，
参数与万象方案一致：

| 参数 | 值 |
|---|---|
| 搭配最大长度 | 6（前文与词各取 5 字查询） |
| 搭配最小长度 | 2 |
| 搭配罚 | −14 |
| 未搭配罚 | −6 |
| 弱搭配罚 | −100 |
| 句末罚 | −8 |

引擎把前文尾部 3 个字交给模型；模型对前文的每个后缀与词的每个前缀组合
取最高分。该分数直接加在词的对数权重上。

## 验证

同一套白霜词库、同一套引擎，只替换 `.gram` 文件：

| 语料 | 旧 essay 模型 | 万象 LTS |
|---|---|---|
| `dicts/eval.txt`（80 句，开发集） | 95.0% | 95.0% |
| 万象 benchmark 留出集（246 句，新闻/小说/散文/科技） | 48.0% | 63.0% |

留出集来自 `rime-wanxiang-data` 的 `rime-schema-compare` 报告 CSV，只取
拼音输入与金句，不随仓库分发。复现时下载报告 CSV 后用同一引擎跑
`gold == 首选`。

体积与性能：

- 文件用 `mmap` 打开，启动时只校验 44 字节头，不解析、不整文件读入。
- 查询是 darts-clone 双数组的按键长遍历，复杂度与文件大小无关；大文件
  只影响缓存命中率。本机 Python 微基准两个模型都在 5 µs/次量级（Python
  开销占主导），Rust 侧以 E2E 的 p50/p95/p99 门禁为准。
- 398 MiB 是磁盘/下载体积；常驻内存随实际触达的页增长，页缓存可回收。

## 许可

万象词库与语法模型采用 CC BY 4.0。分发时保留本说明与上游链接即可；
`tools/fetch_gram.sh` 钉住 release 与 SHA-256，不修改模型文件。
