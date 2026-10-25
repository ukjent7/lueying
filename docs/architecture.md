# 掠影架构

掠影是 Windows 11 的 TSF 拼音输入法，Rust 2024。默认离线运行，不依赖云端。

## 进程与安装

- `lueying_tsf.dll`：TSF COM 薄壳。负责注册、按键预演/应用、异步编辑会话、光标前文本探测、语言栏与模式同步；保留 Ctrl+. 切换中文标点，安全桌面激活时全部按键直通。字符键按当前键盘布局经 `ToUnicodeEx` 翻译，支持 AZERTY/QWERTZ/Dvorak 等非 US 布局。组合期间向 TSF 登记 `ITfCandidateListUIElement` 与 `ITfIntegratableCandidateListUIElement`，宿主（Windows 搜索框、UWP、全屏应用）或读屏软件可读取全量候选、分页与当前高亮，并通过序号选中/提交，声明高亮即待上屏项并显示候选编号；宿主在 `BeginUIElement` 回 `FALSE` 接管绘制时，服务端候选窗自动收起；搜索框调用原样落定接口时提交当前显示文本，不补全剩余拼音。激活时向线程管理器登记 `ITfFunctionProvider`，Chromium/QQNT 据此把 Ctrl+数字等组合键完整交给输入法；触屏键盘可经同一 Provider 取得简体拼音优化布局。激活时即拉起常驻服务，词典与重排模型加载不占首键时间；当前输入框挂 `ITfTextLayoutSink`，宿主滚动或改布局后候选窗重新贴住光标。
- `lueying-server.exe`：常驻服务。持有词典、引擎、用户词库和重排模型；每个宿主会话一份 `Engine`，通过命名管道响应请求。
- `lueying-settings.exe`：独立设置窗。写 `config.ini` 后通知服务重载。
- `lueying-install.exe`：安装到 `%ProgramFiles%\lueying`。复制版本化 x64/x86 DLL（`lueying_tsf-<版本>.dll`、`lueying_tsf32-<版本>.dll`）、注册 TSF、写自启动/卸载项；文件与注册表动作按需提权。

用户数据只落在当前用户目录：配置 `%APPDATA%\lueying\config.ini`，用户词库 `%LOCALAPPDATA%\lueying\user.tsv`。升级安装会一次性接走旧 `pinyin-ime` 目录下的配置与词库，并清理旧自启动项、卸载项和快捷方式。自启动与卸载项写在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Lueying` 与 `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Lueying`；开始菜单快捷方式为「掠影设置」与「卸载掠影」。

管道名是 `\\.\pipe\lueying-v1.<用户哈希>.s<登录会话号>`，行 JSON 请求-应答。SDDL 放行当前用户、AppContainer 与低完整性客户端；服务端用 `Local\` 互斥锁保证同一登录会话只有一个实例。DLL 在服务未运行时从同目录拉起 `lueying-server.exe`，服务不可用时按键直通，避免死键盘。宿主崩溃不会发 `Goodbye`，连接断开时服务端按连接号清理会话、重排缓存与停顿任务。TSF 在组合开始时读取 `GUID_PROP_INPUTSCOPE`，私密字段只关闭学习，不影响输入。

## Crates

| crate | 职责 |
|---|---|
| `lueying-dict` | 文本/二进制词典、签名索引、`.gram` 读取、音节与模糊等价 |
| `lueying-engine` | 切分、词图 Viterbi 束搜、候选、模糊音、错音纠正、用户词库 |
| `lueying-rerank` | 4M 同步重排与 25M 停顿重排的门控/去抖 |
| `chinese-ime-lm` | 内嵌的字符级模型运行时（Apache-2.0） |
| `lueying-protocol` | TSF DLL 与服务的行 JSON 协议 |
| `lueying-server` | 管道服务、候选窗、设置窗、配置与用户词库落盘 |
| `lueying-tsf` | TSF COM 实现、注册、语言栏、键盘映射 |
| `lueying-installer` | Program Files 安装/卸载、x64/x86 TSF 注册 |
| `lueying-e2e` / `lueying-win-e2e` | 引擎 E2E、候选准确率基准、Windows 全链路 E2E |
| `xtask` | 白霜词典物化与发布目录打包 |

## 输入链路

1. TSF 的 `OnTestKeyDown` 把键送到服务端预演；`OnKeyDown` 应用缓存结果并写回文档。
2. 引擎把输入切成音节图，枚举词图节点，用 Viterbi 束搜保留每个终点的若干最优路径；整句备选足够时，默认一页五条里四条是整句，正确读法不会被人名或单字挤出前五；最高分且与首选不同的前缀词留在第二位方便分段上屏。
3. 词条分数由词典词频、字数、用户频次和可选 `.gram` 搭配分组成。
4. 默认开启核心模糊组：`z/zh`、`c/ch`、`s/sh`、`l/n`、`f/h`、`r/l`、前后鼻音。模糊改动按处数罚分，精确常用词锁首位，与首位接近的精确整句同样保护；用户选过一次的整词直接回到首位，分段上屏学到的前缀词不顶掉整句首选，选错可用 Shift+Delete 忘掉。
5. 无整句候选时尝试相邻键替换、漏键插入、多键删除；纠正候选按整串上屏，学习键单独记录。
6. 4M 模型在按键返回前重排可比较候选；25M 模型在静默 120ms 后异步重排，只允许上下文明确的模糊候选越过可信精确首位。
7. Backspace/Delete 与左右方向键按字符移动，候选窗拼音串在光标位于串中时用 `|` 标出编辑位置；空格/数字选词、Enter 上屏原文、Esc 取消组合，Shift+Delete 忘掉高亮候选的学习记录；对齐微软拼音与微信输入法的编辑习惯。
8. 候选窗是无焦点置顶 GPUI 窗口，支持深浅色、鼠标点选、翻页、DPI/多屏贴靠与全屏隐藏；宿主不实现 `GetTextExt` 时退回前台线程的原生插入符，再不行才用视图矩形。Server 进程显式声明 Per-Monitor V2 DPI 感知，避免高缩放与副屏下窗口尺寸/位置被系统虚拟化。首次出现做 140ms 淡入，组合结束做 90ms 淡出，系统开启减少动态效果时直接呈现与收起。

已上屏文本保留最近 128 字作为上下文；焦点丢失或光标跳转时清空。所有宿主会话共享同一份用户词库，学习结果跨应用立即可见；词库按周衰减、上限 2 万条，词条与模糊组统计一起写入 `%LOCALAPPDATA%\lueying\user.tsv`；设置页可经 Server 导出/合并导入 TSV，写入后立即落盘。

## 数据与发布

- `main.pyd`：白霜词典的 mmap 二进制，必需；`main.tsv` 是兼容回退。
- `main.gram`：可选语法模型，默认包不包含。需要时把 rime `.gram` 命名为 `main.gram` 放到 `lueying-server.exe` 同目录，服务启动时自动加载；缺失时退化为纯词频。
- `rerank-4m.safetensors` / `rerank-25m.safetensors`：两档 Apache-2.0 重排模型，默认包包含。
- `xtask pack` 生成发布目录、`version.txt` 与 `manifest.json`，并复制根 `LICENSE`、`licenses/Apache-2.0.txt` 和第三方模型署名。
- 发布包包含 x64 与 x86 两套 TSF DLL；Windows 产物静态链接 CRT，避免 AppContainer/UWP 宿主缺 `vcruntime140.dll`。默认不带 380MB 级语法模型。签名仍在收口；本项目不依赖网络服务，不做自动更新与日志上传（`lueying-server` 只是本机常驻进程）。

## 验证

- `lueying-e2e`：内置词库场景、二进制词典往返、候选窗引擎契约、用户词库、Shift+Delete 学习记录删除、宿主原样落定显示文本、主词典探针、性能分位数、整句准确率、神经重排与候选准确率基准。`--bench-fast` 跑确定性等距子集并并行各语料，`--bench` 跑全量。产物 `target/e2e-report.md` 与 `target/bench-report.md`。
- `lueying-win-e2e`：进程内注册 TSF、激活、注入“nihao+空格”、读回编辑框并验证 Shift 中英切换与 Ctrl+. 中文标点落盘，并校验当前输入框已挂布局监听；同时挂 `ITfUIElementSink`，确认候选 UI 元素登记、更新、结束且能读回首条候选，校验集成候选接口声明高亮即待上屏项，多页时读回页起点并用 `SetSelection` 切页，校验 `GetCurrentPage` 与 `GetSelection` 跟随，再调用 `FinalizeExactCompositionString` 验证宿主原样落定，最后量 5 轮空格到文本落地的全链路延迟（p95 ≤ 250ms）。产物 `target/win-e2e-report.md`；失败时附 TSF 与 Server 日志末尾。CI 另用 i686 构建运行同一套宿主，验证 x86 TSF 的真实加载与输入链路，而不只检查注册表。
- `lueying-install-e2e`：把真实安装器指向隔离的 `LUEYING_INSTALL_DIR`，检查 x64/x86 文件、自启动/卸载项与两套 TSF CLSID，再执行真实卸载并确认清理。产物 `target/install-e2e-report.md`。
- CI：`fmt`、`engine`、`dict`、`windows` 四个 job；windows job 构建 i686 TSF 并做 ARM64 编译检查，x64/x86 全链路与安装/卸载冒烟为阻断项；push 跑快速基准，夜间或手动触发跑全量基准；发布目录只在手动触发或打 tag 时生成。

## 许可

主程序 GPL-3.0-or-later。`crates/lm` 与两档重排模型为 Apache-2.0；白霜词典为 GPL-3.0。随包署名见打包生成的 `licenses/README.md`。
