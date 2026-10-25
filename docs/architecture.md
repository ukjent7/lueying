# 掠影架构

掠影是 Windows 10/11 的 TSF 拼音输入法，Rust 2024。默认离线运行，不依赖云端。

## 进程

- `pinyin_tsf.dll`：TSF COM 薄壳。负责注册、按键预演/应用、异步编辑会话、光标前文本探测、语言栏与模式同步；保留 Ctrl+. 切换中文标点，安全桌面激活时全部按键直通。
- `pinyin-server.exe`：常驻服务。持有词典、引擎、用户词库和重排模型；每个宿主会话一份 `Engine`，通过命名管道响应请求。
- `pinyin-settings.exe`：独立设置窗。写 `config.ini` 后通知服务重载。
- `pinyin-install.exe`：按用户安装。复制版本化 DLL、注册 TSF、写自启动/卸载项；注册表动作按需提权。

管道名是 `\\.\pipe\pinyin-ime-v1.<用户哈希>`，行 JSON 请求-应答。SDDL 放行当前用户、AppContainer 与低完整性客户端；服务端用 `Local\` 互斥锁保证同一登录会话只有一个实例。DLL 在服务未运行时从同目录拉起 `pinyin-server.exe`，服务不可用时按键直通，避免死键盘。

## Crates

| crate | 职责 |
|---|---|
| `dict` | 文本/二进制词典、签名索引、`.gram` 读取、音节与模糊等价 |
| `engine` | 切分、词图 Viterbi 束搜、候选、模糊音、错音纠正、用户词库 |
| `rerank` | 4M 同步重排与 25M 停顿重排的门控/去抖 |
| `lm` | 内嵌的 `chinese-ime-lm` 字符级模型运行时（Apache-2.0） |
| `protocol` | TSF DLL 与服务的行 JSON 协议 |
| `server` | 管道服务、候选窗、设置窗、配置与用户词库落盘 |
| `tsf` | TSF COM 实现、注册、语言栏、键盘映射 |
| `installer` | 按用户安装/卸载与 TSF 注册 |
| `e2e` / `win-e2e` | 引擎 E2E、候选准确率基准、Windows 全链路 E2E |
| `xtask` | 白霜词典物化与发布目录打包 |

## 输入链路

1. TSF 的 `OnTestKeyDown` 把键送到服务端预演；`OnKeyDown` 应用缓存结果并写回文档。
2. 引擎把输入切成音节图，枚举词图节点，用 Viterbi 束搜保留每个终点的若干最优路径。
3. 词条分数由词典词频、字数、用户频次和可选 `.gram` 搭配分组成。
4. 默认开启核心模糊组：`z/zh`、`c/ch`、`s/sh`、`l/n`、`f/h`、`r/l`、前后鼻音。模糊改动按处数罚分，精确常用词锁首位，学过的模糊词可拿回首选的位次。
5. 无整句候选时尝试相邻键替换、漏键插入、多键删除；纠正候选按整串上屏，学习键单独记录。
6. 4M 模型在按键返回前重排可比较候选；25M 模型在静默 120ms 后异步重排，只允许上下文明确的模糊候选越过可信精确首位。
7. 候选窗是无焦点置顶 GPUI 窗口，支持深浅色、鼠标点选、翻页、DPI/多屏贴靠与全屏隐藏；首次出现做 140ms 淡入，系统开启减少动态效果时自动静态呈现。

已上屏文本保留最近 128 字作为上下文；焦点丢失或光标跳转时清空。用户词库按周衰减、上限 2 万条，词条与模糊组统计一起写入 `%LOCALAPPDATA%\pinyin-ime\user.tsv`；设置页可经 Server 导出/合并导入 TSV，写入后立即落盘。

## 数据与发布

- `main.pyd`：白霜词典的 mmap 二进制，必需；`main.tsv` 是兼容回退。
- `main.gram`：可选语法模型，默认包不包含。需要时把 rime `.gram` 命名为 `main.gram` 放到 `pinyin-server.exe` 同目录，服务启动时自动加载；缺失时退化为纯词频。
- `rerank-4m.safetensors` / `rerank-25m.safetensors`：两档 Apache-2.0 重排模型，默认包包含。
- `xtask pack` 生成发布目录、`version.txt` 与 `manifest.json`，并复制根 `LICENSE`、`licenses/Apache-2.0.txt` 和第三方模型署名。
- 默认发布包不带 380MB 级语法模型；安装器、签名与自动更新仍在收口。

## 验证

- `pinyin-e2e`：内置词库场景、二进制词典往返、候选窗引擎契约、用户词库、主词典探针、性能分位数、整句准确率、神经重排与候选准确率基准。`--bench-fast` 跑确定性等距子集并并行各语料，`--bench` 跑全量。产物 `target/e2e-report.md` 与 `target/bench-report.md`。
- `pinyin-win-e2e`：进程内注册 TSF、激活、注入“nihao+空格”、读回编辑框并验证中英切换，再量 5 轮空格到文本落地的全链路延迟（p95 ≤ 250ms）。产物 `target/win-e2e-report.md`；该任务目前在 CI 中为观察项。
- `pinyin-install-e2e`：把真实安装器指向隔离的 `LOCALAPPDATA`，检查文件、自启动/卸载项与 TSF CLSID，再执行真实卸载并确认清理。产物 `target/install-e2e-report.md`。
- CI：`fmt`、`engine`、`dict`、`windows` 四个 job；windows job 另做 i686/ARM64 TSF 编译检查；push 跑快速基准，夜间或手动触发跑全量基准；发布目录只在手动触发或打 tag 时生成。

## 许可

主程序 GPL-3.0-or-later。`crates/lm` 与两档重排模型为 Apache-2.0；白霜词典为 GPL-3.0。随包署名见打包生成的 `licenses/README.md`。
