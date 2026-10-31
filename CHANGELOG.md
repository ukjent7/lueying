# 更新日志

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),版本号遵循语义化版本。

## 未发布

### 新增

- 对外 commit 通道(T15):其他本地工具可借用输入法的上屏能力,不必自己
  做输入法。协议新增 `Request::ExternalCommit { token, text }` 与
  `Reply.commit`,三道门按「用户确认 → 调用方鉴权 → 隐私」顺序判:
  配置键 `commit_channel` 默认关,即用户确认;64 位十六进制随机令牌存
  `%APPDATA%\lueying\commit-token`,只在通道开启后由服务端生成;活动
  会话是隐私字段时一律拒绝。管道沿用主管道 SDDL,远程客户端连不上。
  CLI 新增 `lueying ext commit <文本>`;令牌变化时读回重试一次,覆盖
  开启后的第一次调用。
- PowerToys 命令面板桥接(T16):扩展命令可以从掠影之外触发。宿主方法
  新增 `commands.list` / `command.run`,与动作候选、热键走同一条执行
  路径,隐私字段下拒绝;CLI 新增 `lueying ext run <命令 id>` 与
  `lueying ext commands`。命令面板用「书签」包住 CLI 即可唤起;原生
  扩展模型(.NET WinRT + MSIX)评估后不做,接法见 docs/extensions/powertoys.md。
- 声明式皮肤层(T17):`%APPDATA%\lueying\skin.toml` 浅/深两张表按字段覆盖
  候选窗配色,缺键用主题默认,高对比度整份忽略;改完 reload 生效。SkSL
  用户效果是它之上的高级层,见 docs/skin.md。
- 候选窗 V3 帧级测量(T17):Windows E2E 随服务端跑帧级基准,量两条渲染
  路径的建面、首帧、稳态帧、SkSL 增量与工作集增量,报告
  `target/v3-frames.md` 随 CI artifact 保留;默认路径按数据决策并记录在
  docs/architecture.md(维持 GPU 优先、CPU 兜底)。Skia 体积裁剪同批评估:
  预编译键已是最小可用组合,维持现键,评估记录同上。
- 重排/联想服务缝(T18):停顿重排可换提供者——配置
  `settled_rerank_provider="bypass"` 与 `bypass_exe` 后,静默窗口的置顶
  建议由旁路进程(本地大模型等)给出,连续 3 次失败自动回落内置 25M,
  默认关;联想模块开放外部来源缝(`features::PredictSource`,结果排在
  内置联想之后、拼音过同一道前缀过滤),为本地模型联想预留,默认不装。
- Wasm 扩展层与旁路进程原型(T14):`crates/extwasm` 用 wasmtime 组件
  模型跑通 `probe` 世界,燃料、epoch、8 MiB 内存上限与连续 3 次失败停用
  与脚本层预算对齐;`crates/bypass` 定义 JSON 行协议与 Job Object 限额,
  按需启动的子进程超时即杀;`BypassProvider` 接进服务缝,服务提供者替换
  就绪。嵌入成本在 e2e 的 wasm 场景测量,采纳结论与 V3 帧级测量一起定。
- 扩展开发回路(T12):`lueying ext` 新增 `new` / `pack` / `install` /
  `reload` / `logs` 子命令,覆盖建骨架、打包(`.luext` 行数声明文本块,
  只收 UTF-8 文本)、安装(默认不覆盖,`--force` 覆盖,装完复验清单)、
  重读配置与看日志;`new` 生成的骨架自带 `hello` 命令与一个声明式设置项。
- 无头回放(T12):新增 `lueying ext replay`,在 Linux 上把按键脚本喂给与
  服务端同一套引擎(内置迷你词库、全新用户词库、默认全拼、联想关闭),
  触发词按服务端同一条路径注入动作候选,输出每步上屏、组合与候选;输出
  确定,可直接当回归用例。触发词来自 config.toml `[triggers]` 或
  `--trigger` 旗标,命令行优先。
- 扩展编写指南(T12):新增 `docs/extensions/agent-guide.md`,面向 coding
  agent 的完整开发回路:包规则、按键脚本格式、回放确定性约定、声明式
  设置与收尾自查清单;`docs/extensions/README.md` 增加开发回路一节。
- 配置统一为 TOML(T10):`%APPDATA%\lueying\config.toml` 是类型化 schema,
  未知键在读取时拒绝,坏值收集成「文件层 + 原因」写进日志并在设置页页脚
  提示;按应用覆盖独立到 `apps.toml`,扩展设置进 `[ext.<id>]` 命名空间。
  旧 `config.ini`/`apps.ini` 首次启动自动转成 TOML,备份导入兼容旧块。
- 通用扩展请求(T10):协议新增 `Request::Ext { id, method, params }` 与
  `Reply.ext`,宿主提供 `settings.get` / `settings.set`;读写逐个键按清单
  schema 校验,`settings.set` 合并后保存并立即下发给现有会话。
- 扩展声明式设置(T10):清单 API 2 可声明 `settings`(开关或单选),宿主在
  设置页「扩展 · <名称>」分组里渲染,值存 `[ext.<id>]`;每包最多 32 项,
  键、标题、选项、默认值与未知字段在加载期校验。

- 设置窗无障碍(T9):设置窗接入 AccessKit,把 `settings_view` 的稳定控件 ID
  映射成 UI Automation 树,读屏软件能读到分组、开关、单选、按钮与状态提示,
  并通过 `Click` / `Focus` 操作,与鼠标走同一条处理路径;焦点移动、滚动与
  设置变化后树立即刷新。自定义短语改为设置窗内的原生 `EDIT` 编辑窗,`Tab`
  在编辑区、保存、取消之间循环,保存后自动重载。Windows E2E 用 UIA 读树,
  断言名称、分组与开关,再经 `Invoke` 打开编辑器,用 `ValuePattern` 写入
  短语并保存,读回 `phrases.txt` 确认文本输入与落盘。
- 扩展运行指标(T8):常驻服务每秒把扩展状态、内存占用、派发/作业步峰值、
  超时与内存超限、连续失败与最近错误写到
  `%LOCALAPPDATA%\lueying\logs\ext-metrics.json`;内容不变时只按 30 秒心跳
  重写,配置里启用但待同意或清单损坏的包也列出。新增 `lueying ext top`
  只读该文件,支持 `--file`、`--json` 与 `--watch`;设置页「扩展 →
  扩展运行」显示同一份摘要并可手动刷新。作业步被 8 ms 预算打断计一次
  失败,连续 3 次失败或超限后停用该扩展;E2E 增加普通派发峰值不超过
  预算的延迟门禁,Windows E2E 断言 shot-ocr 的内存与派发指标落盘。
- 扩展授权与审计(T7):声明了 `needs`/`capabilities` 的扩展首次启用前
  要在设置页「扩展 → 扩展授权」里同意,能力翻成人话并标注高敏感/外发;
  读屏或读剪贴板同时申请网络的组合单独强提示。同意记录
  `%APPDATA%\lueying\extensions\consent.json` 固定包内容哈希(清单 +
  入口脚本)、版本与能力,清单/脚本/能力变化后新内容不生效、重新同意
  才热加载,旧版本在此之前继续可用。密码/隐私字段下热键、命令与全部服务调用
  被拒绝;每次服务调用写 `%LOCALAPPDATA%\lueying\logs\ext-audit.log`。
  随包官方扩展由安装器代为同意;`needs` 门控同时加固到 Rust 侧,脚本
  直接调宿主入口也绕不过。E2E 覆盖首次拦截、升级重新同意、热重载门控
  与敏感组合标记。
- 截图 OCR 可选引擎:设置页「扩展 → 截图识别」在系统 OCR 与 PP-OCRv6
  small(det+rec ONNX)之间切换;模型随安装包放在 `{app}\models\ppocr`,
  `%APPDATA%\lueying\models\ppocr` 可覆盖。推理用纯 Rust 的 tract,不随包
  带 onnxruntime.dll;首次识别加载模型,之后复用。`ocr.recognize` 增加
  `engine` 参数,配置键 `ocr_engine=system|ppocr`。Linux CI 拉真模型跑
  det+rec 回归;`tools/fetch_ppocr.sh` 钉住 Hugging Face revision 与
  sha256,词表从 rec 仓库的 `inference.yml` 提取。
- 官方扩展·截图 OCR(T6):`extensions/shot-ocr` 用公开接口串起截图 → 识别 →
  上屏,随安装包分发到 `{app}\extensions`,首次安装复制到当前用户的
  `%APPDATA%\lueying\extensions`(已有目录不覆盖)。`output.deliver` 接线:
  文本按 Unicode 注入当前焦点,不经过键盘布局与输入法组句,Electron 与
  自绘控件同样收 WM_CHAR;换行按回车、制表符换空格,系统拦截时返回明确
  错误。扩展宿主从候选窗线程拆到 `ext_host`,无 UI 的服务也能跑扩展
  (`LUEYING_EXT_HOST`)。验收:Linux 无头黄金测试用仓库里同一份清单与脚本
  断言截图 → 识别 → 上屏的调用顺序与裁剪后的黄金文本;Windows E2E 用
  `Ctrl+Alt+F9` 全局热键跑通真截图与真上屏(CI 镜像缺 OCR 语言包时识别换成
  固定文本,`LUEYING_FAKE_OCR`),并断言编辑框收到识别结果。
- 扩展入口·命令面板(T4b):`config.ini` 的 `panel_hotkey=Ctrl+Alt+Shift+Space`
  (默认值,写 `off` 关闭)注册为宿主总热键,唤出独立的可获焦面板。面板自带
  输入框,字母进一台关闭学习的引擎、用候选匹配命令标题,拉丁标题按子串直接
  匹配;上下键/PageUp/PageDown/Home/End 选择,Enter 或点行执行,Esc 取消,
  唤出前记录前台窗口并在关闭后归还焦点。面板线程空闲时阻塞在通道上,不轮询;
  命令执行与动作候选、用户绑定共用脚本线程的同一条调用路径。Windows 自检
  (`lueying-server.exe --panel-selftest`)用真实按键注入覆盖拼音搜索、方向键
  选择、Enter/Esc 与焦点归还,win-e2e 拉起并断言报告。
- 输入内触发·动作候选(T5):`config.ini` 的 `triggers=bc=shot.run,...` 把
  触发词映射到完整命令 id,整串输入完全一致时命令以候选形式出现(带「命令」
  提示,默认排第 2 位),数字键、方向键+空格或点选执行命令,不上屏文字、
  组合清空;空格与 Enter 照旧上屏文字,字面原文候选缺了就补在页尾,用户的
  字母退路始终可达。连续两次选字面原文后该触发词降频(借用户词库同键负分,
  按周衰减);用既有置顶机制置顶后动作候选排首位。服务端在脚本线程维护
  命令标题表,选择经请求线程投递执行,与热键共用一条调用路径;E2E 覆盖
  空格、Enter、数字选择、点选、负反馈、置顶与只做整串匹配。
- 扩展入口·绑定表与全局热键(T4a):用户绑定表由宿主管理,`config.ini` 的
  `bindings=Ctrl+Alt+O=shot.run,...` 把组合串映射到完整命令 id,宿主强制
  两个修饰键、拒绝系统保留组合,表内冲突报已绑定的命令;`when` 过滤(any/
  editable/composing)按触发时刻的会话状态放行。热键在扩展线程注册与收取,
  WM_HOTKEY 经消息泵兑现:`hotkey.press` 让扩展挂起等触发(async 循环里
  反复调用),用户绑定命中后执行命令并记日志;换扩展或改绑定时 OS 注册
  整表重建,不会残留旧组合。Windows 集成测试用真实按键注入覆盖注册冲突、
  触发派发与 press 兑现。
- 异步命令的被打断语义闭环:quickjs-ng 对不可捕获的中断异常跳过 Promise
  capability reject,被步进预算杀掉的作业永远不结算;宿主在泵里检测到打断
  后清零挂起记账(等价于丢弃本轮被杀的根任务),并去掉按挂起数跳泵的旁路,
  保证被打断后定时器与服务结算照常推进;E2E 断言两泵为空、记账清零与
  中断后宿主可用。
- Windows 服务提供者:capture 用 GDI 截虚拟桌面或指定区域写 32 位 BMP,ocr
  走 Windows.Media.Ocr(请求语言不支持时回退用户语言并报实际语言),hotkey
  做注册/冲突/注销语义,notify 落日志;output 与 panel
  是 T6/T4 的占位,回明确错误文本。服务端 worker 装填提供者;CI 的 Windows
  job 增加服务端 lib 测试,跑真截图与真识别的提供者集成验证。
- 服务缝 v1(T3):六项宿主服务 capture/ocr/output/panel/hotkey/notify 走
  `lueying.<service>.*` Promise 接口,调用面按清单 `needs` 门控,接口记录在
  `lueying-extsvc` 的 WIT 文件;提供者经注册表装填,结果走完成队列异步回传,
  缺提供者与未声明服务当场报错;E2E 用假提供者覆盖调用链、面板延迟结算、
  热键冲突、缺提供者与未声明拒绝。
- 扩展平台验证(任务书 V1–V8)进入 Windows E2E:V1 用 GDI 画中文样张走
  Windows.Media.Ocr(普通 push 测语言包缺失行为,手动触发先装 zh-CN 能力
  测质量),V3 出二进制体积基线,V5 实测 UIA 判断焦点可编辑性(含只读态
  的结论),V8 实测 RegisterHotKey 注册/冲突/保留键;V2/V4/V7 记录方法与
  依赖,等 T3/T4/T9 落地后补齐。报告写 `target/ext-verify-report.md`,
  随 CI artifact 保留。
- 异步命令:T2 步进模型。命令可以返回 Promise,`lueying.timeout(ms)` 提供
  宿主时钟延时;宿主按步进预算泵 QuickJS 作业队列(每步受派发时限,单轮
  至多 256 步),等待原生服务与定时器不计时;异步失败计入连续失败计数,
  E2E 覆盖挂起、到点结算、死循环作业打断与异常结算。
- 脚本命令与多扩展:API 2 清单声明激活方式(触发词/建议热键/命令)、宿主
  服务(`needs`)与权限(`capabilities`);`lueying.command` 命令宿主按完整
  id `<扩展>.<命令>` 调用,只能执行清单里声明过的命令;`config.ini` 的
  `extensions=a,b,c` 按序加载多个扩展,命令包按激活事件懒加载、空闲 30 秒
  卸载,卸载时 Scope(命令/触发词/热键)整体回收;旧键 `script=` 继续可读。
- 扩展分层模型:共享清单与权限模型(`lueying-extmeta`),一个包可以同时带
  外观、逻辑 WASM 与进程外能力组件;旧版顶层 `api`/`main` 外观包保持兼容。
  分层设计见 `docs/extensions/layers.md`。
- 运行期脚本扩展内核(`lueying-extjs`):QuickJS 沙箱、版本化场景协议
  (面板状态进,主题色与三个 SkSL 槽位出)、内存/栈/时限预算、隐私字段跳过、
  连续失败停用与按内容哈希的热重载;Linux E2E 覆盖坏清单、坏补丁、死循环、
  内存暴涨、隐私与热重载。服务端按 `config.ini` 的 `script` 键加载,派发在
  独立线程里,候选窗应用主题色与 SkSL,高对比度与隐私字段跳过;Windows CI
  有运行时自检。设置页「运行期脚本」新增「选择」与「目录」:列出已安装的包,
  可直接启用、切换或关闭,也能打开扩展目录。
- 设置窗迁到原生窗口:普通 Win32 窗 + Skia 光栅 + GDI 贴图,不再依赖 GPUI;
  页面模型下沉到平台无关的 `lueying-server::settings_view`,画面与命中在
  `lueying-render::ui`,Linux E2E 直接断言设置页结构与交互结果;
  Windows CI 跑设置窗自检(建窗、画一帧、贴一次、读回像素)。
- 摘掉 GPUI/zed 的 git 依赖:候选窗、模式徽标、设置窗三条路都走原生窗口,
  依赖树里不再有 zed 仓库,锁文件从 609 个包缩到 91 个。
- 候选窗 CPU 兜底:机器上没有硬件 D3D12(虚拟机、远程桌面)时,候选窗改走
  CPU 光栅 + 分层窗口,画面与 GPU 路径一致,不再整块消失;运行中设备被系统
  摘掉(驱动重启、GPU 挂起)也会换到这个窗口接着弹,不再整块停用。
- Skia 候选窗:Win32 无焦点窗 + DirectComposition 交换链 + Ganesh-D3D12
  (只用硬件 D3D12 适配器,建不出设备时退到 CPU 光栅);摆位/DPI/全屏
  避让与模式徽标共用,绘制、命中测试与菜单几何在平台无关的 `lueying-render`,
  Linux E2E 光栅像素断言,Windows CI 管线自检。淡入/淡出、翻页滑动、悬停、
  点选、滚轮与右键菜单与原 GPUI 路径同参数(含「减少动态效果」)。
- 候选窗外观设置:排列(横排换行/竖排单列)、候选字号三档、主题色六档;
  高对比度主题始终跟随系统色,候选字号只缩放候选行。
- 自定义标点:`%APPDATA%\lueying\punct.txt`,一行一条 `键=输出`,
  表内固定按映射上屏,表外走默认中文标点。
- 按应用配置:`%APPDATA%\lueying\apps.ini`,按宿主 exe 名覆盖中文标点
  与学习开关,会话上线时套用。
- 配置备份:设置里一键把配置、短语、标点、按应用与用户词库打包成一份
  文本,导入时逐块恢复并自动重载。
- 更新渠道:设置「关于」里可选稳定或测试;测试渠道跟进预发布 Release。
- 每周依赖安全审计工作流(`cargo audit`,定时)。
- 架构文档:进程拓扑、引擎排序层次、用户数据文件与质量门禁。
- 双拼:小鹤、自然码、微软、搜狗四套键位,与全拼共享词库、学习记录与整句解码。
- 自定义短语:`%APPDATA%\lueying\phrases.txt`,按编码与简拼匹配,加载时建索引。
- 辅助码:拼音后按 `` ` `` 打 1–2 位五笔码,内置 7900 余个常用字的码表。
- 下一词联想:按本机上屏词序,用转移与三词上下文补全。
- 扩展 SDK(`lueying-ext`)与扩展文档;扩展在编译期注册,不加载运行时 DLL。
- 设置:输入方案、辅助码、下一词联想、短语编辑、检查更新;窗口按可用高度收缩。
- 共享语料基准并行给出产品链路(4M 同步 + 25M 停顿)与仅 25M 两种排序的准确率。
- 用户文档:安装、使用、设置与排障。
- 服务日志:`lueying-server.exe` 的启动、降级与请求失败写入
  `%LOCALAPPDATA%\lueying\logs\server.log`,超过 1 MB 滚动一份。
- 设置「关于」里的「引擎状态」行:词典来源与规模、语法与重排模型的加载情况;
  诊断报告同样附上,缺模型时一眼可见。
- 服务日志记录耗时超过 100 ms 的请求(类型与耗时),配合诊断导出定位卡顿环节。
- 设置「关于」里的「诊断信息 → 导出」:版本、配置、模糊组统计与服务日志末尾
  拼成一份文本,排障时直接附上。

### 变更

- 结构梳理:全仓单文件压回 1000 行内、目录深度 ≤3,拆、合、挪并举。
  - 拆:`engine/input` 按键分发/组合编辑/上屏学习/候选重算四份 inherent
    impl;e2e 渲染族按面板/效果/界面拆三份;server 的页面构建
    (`settings_pages`)、扩展方法族(`server_ext`)与配置备份
    (`settings_backup`);`tsf/service` 的键汇与提供者(`keysink`/`providers`);
    设置窗控件绘制(`ui_paint`);`xtask` 按子命令拆成 dict/pack/manifest;
    `install-e2e` 的内嵌模块挪出为 `imp.rs`。
  - 合:e2e 十五份检查表、三份临时目录与像素读回收进 `harness.rs`,报告
    表格统一转义(此前 11 处 `|` 会撑破 Markdown 表);分位数函数 `pct`
    收敛为 `shared` 一份;五个单方法 provider 的入口守卫提取为
    `services::one_method`;`bypass-echo` 挪出 `src/bin`,消除唯一的
    第四层目录。
  - 清:删除七处全仓零引用的死代码、三处冗余 `windows-core` 直连依赖、
    十九处指向已删除 GPUI 路径的注释;OCR 引擎分段控件补进键盘导航
    (Tab 可达后左右键此前无效);测试挂载统一为 `src/*_tests.rs`。
- 更新包校验改为进程内流式 SHA-256(T11 第一步):不再经 `certutil`
  子进程算安装包摘要,百 MB 安装包按 64 KiB 流式计算,失败原因直接可读;
  哈希实现复用扩展内容指纹的零依赖 SHA-256。
- 签名更新清单(T11):Release 附 `lueying-manifest.json` + `.sig`
  (Ed25519,客户端内置公钥)时,镜像、大小与哈希以验签后的清单为准,
  更新器直接拿清单哈希;清单验不过就地停止更新,缺失则退回 `.sha256`
  流程。新增 `xtask update-manifest` / `sign-manifest`,发布工作流在配置
  `LUEYING_SIGN_KEY` secret 后自动生成并签名清单。
- 候选窗默认走 Skia 管线,GPUI 候选窗路径已移除;`ui_backend` 配置键随之
  失效(旧配置里的该键会被忽略)。
- 模式徽标从 GPUI 浮层换成原生分层窗口:Skia CPU 光栅出帧,
  UpdateLayeredWindow 上屏,不依赖 GPU,在没有硬件 D3D12 的机器上也照样显示;
  候选窗在同一侧时徽标躲到另一侧,两者不互相遮挡。
- 长句(30 字节以上)的模糊展开只保留单点变体,首键延迟不再随句长放大。
- 没有左邻上下文时跳过 4M 同步重排:候选先按引擎顺序上屏,停顿后仍由 25M 重排。
- 共享语料、整句评测与重排门禁统一到同一套归一化。

### 修复

- 管道请求行加上 1 MB 上限;不发换行的对端不再能无限占用内存。
- 自定义短语文件带 UTF-8 BOM 时首行失效。
- 辅助码过滤后候选区间被错误截断。
- Windows 上因保留设备名(`aux.rs`)导致的检出失败。

## 0.1.0

首个可用版本:全拼与简拼混输、模糊音、整句词图解码、语法模型、神经重排、用户学习、TSF 接入与候选窗。
