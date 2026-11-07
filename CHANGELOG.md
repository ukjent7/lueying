# 更新日志

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循语义化版本。
发布工作流按 tag 从本文件提取对应版本段，作为 GitHub Release 的说明。

## [Unreleased]

### 新增

- 解码常数自动调参（`tools/tune_decoder.py` + `tune` 工作流）：坐标系搜索，每个常数在邻域各取一点并行跑开发集基准，「整句首选变好、干净全拼不退」的候选带证据提交。
- 词典自动跟随：每天检查 lueying-dict 的正式 Release，新版本先过基准门禁再自动更新 `fetch_dict.sh` 的钉住标签与校验和；发版节奏旋钮在词典仓（周一定时，默认每周，两次间隔半年兜底）。
- 用户纠错专项语料 `eval-user-v4`（50 句，出题在前、采集在后，分词空格剥除），进 dict-bench 矩阵。
- 剪贴板历史官方扩展：宿主后台维护最近 200 条纯文本复制记录，`Win+Shift+V` 唤出胶囊面板（显示最近 60 条），选中即写回剪贴板并注入当前焦点；OCR 与语音识别结果自动带 `OCR`/`语音` 标签沉淀进历史，不额外申请剪贴板权限。
- 原生扩展可订阅候选窗状态（`events: ["candidate-state"]`）：宿主推送可见性、位置与组合内容，扩展可据此做贴着候选窗的伴随界面；文字需要 `candidate-text` 权限，私密字段不带文字。
- 离线语音输入官方扩展：`Ctrl+Alt+V` 录制 8 秒 16 kHz WAV，交给原生 `sensevoice` 扩展做纯 CPU 识别后上屏；音频录制与 ASR 作为宿主服务开放。
- 设置中心「扩展」页新增「允许扩展自由联网」开关：开启后不再逐域名校验清单白名单，扩展仍需声明 `network` 并经用户同意安装。

- 按键重排升级到 chinese-ime-lm 的 8M/25M 选词模型：8M/25M 均随基础安装包分发；两者直接读取上游 `.ort` 图、`model.json` 与 `vocab.json`，用树内纯 Rust 推理，不引入 ONNX Runtime 原生依赖。上游同池评测有上文 top-1 分别为 90.4% / 91.9%（旧 4.25M / 24.9M 为 79.5% / 87.0%），无上文 8M 为 83.0%（旧 4.25M 为 76.1%）。
- 评测工具：共享语料基准改为只产出逐条 TSV，`tools/bench_shared.py summarize` 汇总成 JSON，`diff` 给出 McNemar 精确检验与被改坏的样本，CI 按档位和分片并行；E2E 补上纠错候选必须报告 `trusted_dictionary_hit=false` 的检查。
- 词库导入：设置窗「词库」页的「用户词库 → 导入」除本机导出的原生格式外，还能读标准 TSV 词表（词/拼音/权重）与纯汉字词表；UTF-8、UTF-16、GB18030 都能解码，拼音统一成引擎学习键，权重按对数折算成词频，纯汉字词表的读音从主词典反查。
- 云同步:设置窗「词库」页可把配置、自定义短语、自定义标点、按应用配置与用户词库经 WebDAV 在多台机器之间同步(HTTPS + Basic 认证);远端没有文件就上传,两边都改过则弹窗裁决用哪一份;同步地址与账号留在本机,口令单独存在本机文件里。
- Linux Wayland 外壳 `lueying-linux`：实现 `zwp_input_method_v2` 与 `zwp_virtual_keyboard_manager_v1`，键盘 grab 后跑完整引擎，未消费按键经虚拟键盘回送；候选窗用 `tiny-skia` 软件绘制到 `wl_shm` 的输入法 popup，支持鼠标点选、翻页与浅/深配色；配置与用户数据走 XDG 路径。

### 变更

- 整句纠错放开长度上限：手误通道此前只在 ≤24 字母的输入上生效，长句只靠模糊音兜底；用户实测 31 句带错里有 21 句的错误全部落在这条线上。现在全程开启，长句仍限每词一处手误，≤30 字母的输入放宽到一处词内两错。
- 工具链与依赖现代化：CI 单测换 `cargo-nextest`（文档测试单独保留）；每周审计换 `cargo-deny`，漏洞、依赖来源与重复版本一起管，替代 `cargo-audit` 与手写的锁来源检查脚本；常驻服务与设置窗的分配器换 `mimalloc`。
- 词典换成自研的掠影词典（[lueying-dict](https://github.com/ukjent7/lueying-dict)）：词频来自开放语料，读音由 g2pW 在真实句子里定音，万象和白霜只提供新词候选的词形；二进制格式升级为 PYD2，同等词条体积减少约 42%。`xtask dict` 与 `tools/fetch_frost.sh`、`tools/fetch_gram.sh`、`tools/fetch_collocation.sh` 一并删除，词典与搭配表统一由 `tools/fetch_dict.sh` 下载。
- 候选窗背板优先走 Windows 11 文档化系统背板（`DWMWA_SYSTEMBACKDROP_TYPE` 的 `DWMSBT_TRANSIENTWINDOW`，DWM 合成器托管毛玻璃、底色随系统深浅色），Win11 22H2 之前的系统回退未文档化的 accent 亚克力；macOS 侧液态材质（`NSGlassEffectView`/`NSVisualEffectView`）与三端共用的 `lueying-render` 无头几何不变。
- 删除 `crates/scene` 声明式场景运行时及其解释器、粒子池、精灵与回放：候选窗动效统一由 `lueying-render` 的 Spring 阻尼弹簧驱动，设置页只保留「弹性动效」开关。
- 删除 `crates/bypass`：原生扩展宿主并入 `lueying-extwasm::native`，保留常驻进程、空闲回收、Job Object 限额与 AppContainer 沙箱；移除一次性旁路进程与「停顿重排提供者」配置。
- 核心只保留全拼、简拼与全简混输；双拼、五笔辅助码与形码继续作为官方编译期扩展（`lueying-features`）提供，不再进入核心词图。
- 用户词库取消词条时间衰减：主词典已有词首次选择记两次使用，词典外新词首次只记一次，连续使用后阶梯晋级；只有 Shift+Delete 能手动删除。
- 手误与模糊音纠错候选带 `correction` 标记，界面在候选右侧以灰字提示「纠自 <原拼音>」，准确输入仍优先。

- `lueying-lm` 同步上游 `reference` 0.2.1 的批式推理：一次重排的候选共享前缀只算一次，矩阵乘按 8 行分块，词表归一化同样成批；4.25M 单次决策 12.1→4.1–4.2ms、24.9M 72.3→22.7–22.9ms，输出逐位不变。保留词表外字符过滤与 `best_where_margin`。
- TSF 与服务的管道协议从行 JSON 换成长度前缀 postcard 二进制帧(4 字节小端长度 + 编码体,单帧 1 MiB 上限);管道名升到 `lueying-v2`,两端同包分发,无兼容期。
- 用户词库改为整份快照原子替换:临时文件写完落盘再改名,进程中途退出或掉电只留上一份完整快照;删掉 SQLite 与旧 TSV 迁移,文本导入导出与配置备份不变。
- 打字路径:会话独占锁只护引擎状态,候选窗、徽标与扩展宿主只读发布的会话视图;4M 按键重排移到后台线程,按键不再等模型;候选窗与徽标改为事件驱动等待;TSF 管道客户端改重叠 I/O,单次请求 1500 ms 上限。
- 候选窗、模式徽标与命令面板统一走 Direct2D/DirectWrite,删除 Skia 依赖与预编译命中门禁;候选窗 GPU 管线与 WARP 兜底不变。
- 运行期扩展外观补丁只保留主题色,删掉三个 SkSL 效果槽位与 `.luext` 的 `.sksl` 白名单。
- 运行期扩展从 QuickJS 脚本切换到 Extism 插件沙箱(宿主 Extism 运行期,Cranelift JIT):扩展就是标准 Extism 插件,别的 Extism 宿主写的插件、官方 PDK 编出来的插件都能直接放进扩展目录;扩展用 Rust(`lueying-extguest` SDK)同步编写,每个扩展独立 worker 线程,阻塞服务调用只在 worker 上等待;清单升级到 API 4(命令按命令 id 导出,外观走 `lueying_appearance`,宿主服务从 `extism:host/user` 导入),同意指纹纳入 `.wasm`,`.luext` 包格式升级为 v2(内嵌 Wasm 字节块)。
- `lueying ext new` 生成 Rust guest crate 骨架;`lueying ext pack` 要求清单声明的 `extension.wasm` 存在。
- 官方 `shot-ocr` 改为 Rust guest;CI 增加 wasm32 构建,文档与扩展规范同步更新。
- 系统底线收紧为 Windows 11 24H2,x64 单架构(ARM64 宿主经 x64 模拟加载);删掉 32 位 TSF DLL 与旧配置迁移。
- CI 拆快慢关卡:push 只跑构建、静态检查与体积门禁,Windows 全链路 E2E 移到定时任务与发版前。
- 依赖升级:`ed25519-dalek` 3.0;令牌生成直连 `getrandom`,移除 `rand_core 0.6`、`skia-safe` 与 `rusqlite`。
- 移除 `windows-registry`,注册表读写统一走 `windows` 的 `Win32_System_Registry`。
- 工程分层:TSF、常驻服务、安装器与 Windows E2E 挪进 `platforms/windows/`,平台无关的引擎、语言模型与扩展宿主留在 `crates/`。
- 设置窗 Slint 换 femtovg GPU 渲染(winit 后端,无头与 VM 环境自动回退软件渲染);补全 Cargo.lock 缺失的 slint 闭包。
- 树内语言模型副本更名 `lueying-lm`(v0.3.0);重排上文剥掉词表外字符,25M 模型整句有上文 top-1 从 87.0% 抬到 89.9%(上游 chinese-ime-lm #91)。

### 修复

- TSF 的 `build.rs` 把 MSVC 专用的 `/IGNORE:4104` 链接参数发给了所有目标;Linux 上只要真的去链接含 TSF 的产物(比如新加的文档测试)就会失败,现在只在 Windows 目标生效。
- 设置窗保存扩展密钥后不再卡死:刷新界面时不再握着状态锁去取同一把锁。

## [0.1.0] - 2026-11-01

首个公开版本。

### 输入

- Windows TSF 文本服务 + 常驻服务进程，64 位与 32 位宿主都可加载。
- 全拼、简拼、整句词图解码、模糊音、手误纠正与末音节补全。
- 用户词库：选词提频、同键负分、自动造词、字级搭配、词级转移、三词窗口与按应用学习。
- 候选右键置顶、跳过，用户词库导入导出与配置备份。
- 双拼（小鹤、自然码、微软、搜狗）、五笔辅助码、自定义短语、下一词联想。
- 表情、英文混输、简繁转换、快捷输入（日期、时间、数字读法、算式）与符号表。
- 邮箱与网址整串输入，`u` 模式按码点问字。

### 候选窗与设置

- Skia 候选窗：GPU 优先、CPU 分层窗口兜底，设备被摘时自动换手。
- 横排/竖排、三档字号、六种主题色；高对比度跟随系统，减少动态效果时走终态。
- 逐显示器 DPI、多屏摆位、屏幕边缘翻转与全屏避让；鼠标点选、右键菜单。
- 设置窗：左栏分类、跨页搜索、候选窗实时预览、UIA 读屏树与键盘导航。
- 窗口内编辑自定义短语与标点，按应用覆盖标点与学习，诊断信息导出。

### 扩展

- 编译期扩展：输入方案 (`Scheme`) 与候选模块 (`Module`)。
- 运行期 QuickJS 脚本扩展：主题色与 SkSL 效果槽位、命令宿主、声明式设置、权限同意与审计。
- 官方截图 OCR 扩展，可选系统 OCR 或按需安装的 PP-OCRv6 模型。
- 4M 每键同步重排随包分发；25M 停顿重排在设置页按需安装。

### 质量与发布

- 真实 TSF 宿主 E2E、32 位 E2E、Inno 安装/升级/卸载 E2E。
- 共享语料准确率门限、按键延迟门限、内存与发布体积门限。
- 每周依赖漏洞审计与依赖来源检查。
- 更新清单 Ed25519 签名；配置证书后安装包使用 Authenticode 签名。

### 已知边界

- 仅支持 Windows 10 1809 及以上 x64；ARM64 原生宿主需要 ARM64X TSF DLL，暂不支持。
- 界面与文档目前只有中文。
- 不内置云输入、语音、翻译或账号同步；相关能力留给扩展。
- Windows 安装包与更新清单签名为可选流程，未配置证书时发布未签名产物。