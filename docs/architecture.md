# 掠影架构

面向想改代码的人:进程怎么分工、一次按键怎么走、数据放在哪、质量靠什么守住。
实现细节以代码为准,本文只讲边界与取舍;配置与操作见[使用](usage.md)与[设置](settings.md)。

## 进程拓扑

```
lueying_tsf.dll(薄壳,每个宿主进程一份)
    │  命名管道 \\.\pipe\lueying-v1(名字带用户哈希与登录会话号)
    ▼
lueying-server.exe(常驻)
    ├─ 引擎会话(每宿主线程一个,共享词典/gram/用户词库)
    ├─ 候选窗(Skia:Win32 无焦点窗 + DirectComposition + Ganesh-D3D12)
    ├─ 模式徽标(原生分层窗:CPU 光栅 + UpdateLayeredWindow)
    ├─ UIA 左邻文本回退线程 / 25M 停顿重排线程
    └─ 日志(%LOCALAPPDATA%\lueying\logs\server.log)
lueying-settings.exe(设置窗:原生窗口 + Skia 光栅,经管道与 Server 通信)
lueying-update.exe(更新器:下载 setup.exe,校验 SHA-256 后静默安装)
lueying-user.exe(安装期用户状态:输入法列表、HKCU Run、清理)
```

TSF DLL 只带 `lueying-protocol`;`KeyMsg → Key` 的转换放在 Server 边界,
引擎与 GUI 依赖不进宿主进程。`lueying-engine::types` 是宿主与模块共享的 DTO。

## 引擎

音节图 + 词图 Viterbi,同一输入的所有切分并存(`query_builder`,束宽 7)。
排序层次:词典权重 → `.gram` 搭配分(`weight × (命中 − baseline)`,W=0.35)→
用户词库提频/负分 → 字级搭配(`@context`)与词级转移(`@transition`)→
4M 模型每键同步重排(有左邻上下文时)→ 25M 模型静默 120ms 后异步重排。
手误纠正只在精确/模糊都出不了整句时运行,6ms 预算。

词典由白霜(rime-frost)五表生成,`xtask` 物化为 mmap 二进制 `main.pyd`,
坏损时降级 `main.tsv` 再降级内置迷你词库。模型由 `tools/fetch_*.sh` 按提交与
SHA-256 钉住;gram 或重排模型缺失时引擎照常出词频候选。

## 扩展契约

`lueying-engine::module` 定义 `Module`/`Manifest`/`Capability`,扩展点是
候选生成后(`candidates`)与上屏前(`commit`);`lueying-engine::scheme` 定义
`Scheme`,负责「击键 → 音节」。模块编译期注册(`crates/server/src/modules.rs`),
运行期不加载 DLL。第一批扩展在 `crates/features`:双拼、自定义短语、辅助码、
下一词联想。

运行期脚本扩展走另一条契约(`lueying-extjs`):包目录 + `manifest.json`,
QuickJS 沙箱里同步派发场景状态,脚本只回主题色与三个 SkSL 槽位,也可以提供
命令(`lueying.command`)。清单与权限模型在 `lueying-extmeta`:API 2 脚本清单
声明入口、激活方式(触发词/建议热键/命令)、需要的宿主服务与权限;分层设计见
`docs/extensions/layers.md`。状态不含候选文字与宿主名,密码/隐私字段不派发,
内存、栈、初始化与单次派发都有上限;热重载保留上一版可用的宿主。服务端按
`config.toml` 的 `extensions = ["a", "b"]` 按序加载多个扩展,命令包按激活事件懒加载、
空闲 30 秒卸载;派发在独立线程里,候选窗线程只收补丁并编译 SkSL;补丁没给的
槽位继续用用户效果目录。设置页只读清单列出已安装的包,选择结果写回
`extensions` 键;它不改音节切分,也不碰候选,只画装饰层。清单里的声明式
`settings` 由宿主渲染成设置页控件,值存 `[ext.<id>]`;外部调用走通用
`Request::Ext { id, method, params }`,宿主目前提供 `settings.get` 与
`settings.set`。

## 用户数据

| 文件 | 内容 |
|---|---|
| `%APPDATA%\lueying\config.toml` | 全部设置(TOML;未知键拒绝,坏值记日志) |
| `%APPDATA%\lueying\phrases.txt` | 自定义短语(`短语<TAB>编码`) |
| `%APPDATA%\lueying\punct.txt` | 自定义标点(`键=输出`) |
| `%APPDATA%\lueying\apps.toml` | 按应用覆盖(`[exe]` 节 + `punct`/`learning`) |
| `%LOCALAPPDATA%\lueying\user.tsv` | 用户词库(选词、造词、搭配、置顶) |
| `%LOCALAPPDATA%\lueying\logs\server.log` | 服务日志(超 1MB 滚动) |

设置窗保存 `config.toml` 后发 `ReloadConfig`,Server 重读全部文件并把新配置
下发给所有会话;按应用覆盖在会话上线(`Hello`)时套用。设置里的「配置备份」
把上述文件打包成单份文本(按行数声明块长度),导入时逐块写回、词库走管道合并。

## 候选窗

候选窗是 Skia 画的:独立线程自建 Win32 无焦点窗(WS_EX_NOACTIVATE|TOOLWINDOW|
TOPMOST),画面由 DirectComposition 交换链给出,Ganesh 走 D3D12。D3D12 只用硬件
适配器:软件适配器(WARP、Basic Render Driver)的设备在 Ganesh 下会掉设备,
Skia 拿着空命令表句柄继续走就崩。建不出设备就退到 CPU 兜底:换一个分层窗口
(WS_EX_LAYERED),同一套绘制代码落到 CPU 光栅表面,再经 `UpdateLayeredWindow`
上屏——虚拟机与远程桌面这类没有硬件 D3D12 的机器上,候选窗照样弹得出来,只是
每帧多花一点 CPU。设备中途被系统摘掉(驱动重启、GPU 挂起)也走同一条兜底路:
另建一个分层窗口接手、旧窗口销毁,候选窗继续弹;换手前先让 Skia 放弃 GPU
上下文,析构时就不会再碰已经没了的设备。只有 CPU 兜底也建不起来(没有桌面会话)
时才停用候选窗。空闲停靠在虚拟
桌面外。排列有横排(自动换行)与竖排;候选字号三档只缩放候选行;主题色预设只改
强调色,高对比度主题始终用系统色。窗口宽度按全部候选估宽,高度按当前页行数
估算,随 DPI 每显示器计算。动效:展开 140ms、上屏淡出 90ms、翻页 90ms 横向
滑动,全部可被按键打断,「减少动态效果」时走终态。

绘制、命中测试与菜单几何都在平台无关的 `lueying-render`(Linux E2E 光栅像素
断言),窗口层只把命中翻译成协议请求。摆位、DPI 与全屏避让在 `ui_place`,
模式徽标共用同一套。徽标不走 GPU:它是分层窗口上的预乘 alpha 位图,Skia CPU
光栅出帧、UpdateLayeredWindow 上屏,在没有硬件 D3D12 的机器上也照样显示;
候选窗在同一侧时徽标躲到另一侧,两者不互相遮挡。Windows CI 以
`LUEYING_SKIA_SELFTEST` 自检整条管线:光栅
路径与服务端同进程跑,给出确定性像素断言与字体断言(DirectWrite 字型必须真的
画进画面);换手(旧窗口换成 CPU 分层窗口)单独走一遍,断言新窗口建起来、旧
窗口销毁、画面与上屏都还在;Ganesh-D3D12 路径另起子进程跑——没有硬件适配器或
驱动异常的机器上 GPU 管线会把进程带崩,隔离在子进程里,崩了也不影响输入法,
日志如实记下原因。

默认渲染路径按 V3 数据决定,测量先行:服务端按 `LUEYING_SKIA_BENCH` 在启动时
跑帧级基准(CPU 光栅本进程、Ganesh-D3D12 子进程,崩溃隔离与自检同款),量
建面、首帧、稳态帧、SkSL 帧增量与基准期间工作集增量,报告落 `target/v3-frames.md`
随 CI artifact 保留。**决策(2026-10-31):维持「GPU 优先、CPU 光栅兜底」。**
依据:CPU 光栅稳态帧在毫秒级(基准数据见 artifact),单独当默认完全够用;但
GPU 存在时它把每帧的栅格化让给硬件,打字热路径少抢 CPU,首帧也更低;CPU 兜底
只在建不出硬件 D3D12 或设备被摘时启用,画面与上屏已由换手自检验证等价。若
后续基准显示 CPU 稳态超预算或 GPU 首帧劣化,以数据为准重开这个决策。

配色上有声明式皮肤层:`%APPDATA%\lueying\skin.toml` 浅/深两张表按字段覆盖
主题色(键名与内部主题字段一致),高对比度下整份忽略;SkSL 用户效果是它之上
的高级层,只管动效,颜色见 [skin.md](skin.md)、效果见 effects.md。

Skia 体积裁剪评估(2026-10-31,结论:维持现键,不再投入):预编译键
`d3d-ganesh-jpegd-jpege-pdf-textlayout` 已经是 rust-skia 0.156 发布集里唯一
同时满足「D3D12 后端 + textlayout」的 x86_64 Windows 资产(压缩包 24.4 MiB);
该发布集里所有带 `textlayout` 的 Windows 资产都强制捆着 `jpegd-jpege-pdf`,
没有可命中的「textlayout 不带 pdf」资产——这就是 Cargo.toml 里 jpeg/pdf
「不是功能需求,是命中预编译键的必要条件」的原因。textlayout 本身不可去:
形状排版靠它,且只有带它的预编译包才带 `icudtl.dat`(`embed-icudtl` 要用)。
自建 Skia 理论上可裁:上下界参考是「无 textlayout 资产 9.4 MiB ↔ 现键
24.4 MiB」,即压缩口径最多省约 15 MiB,静态链接进 exe 后占比更小;代价是
每次升级都要自编全平台二进制、维护交叉编译与哈希,收益不抵。等上游发布
`textlayout` 不带 `pdf` 的 Windows 资产、或候选窗改用 graphite 后端时重新评估。

## 设置窗

设置窗是独立的 `lueying-settings.exe`:普通不透明 Win32 窗(客户区 460×650
逻辑像素),Skia 在 CPU 上光栅出帧,再经 GDI `BitBlt` 贴到客户区——它不需要
GPU,在没有硬件 D3D12 的机器上也照样开。页面模型在平台无关的
`lueying-server::settings_view`:配置铺成一页(分组、设置行、开关、分段组、
按钮),点一下改配置、左右键改分段值、Tab 走焦点顺序,都在这一层;画面与命中
在 `lueying-render::ui`,与候选窗共用同一套排版与像素断言,Linux E2E 因此
能直接断言设置页的结构与交互结果。平台侧只剩「要动文件系统或常驻服务」的
动作(备份、导入导出、词库、自动更新),在 `settings_actions`。
窗口自检 `--selftest` 建窗、画一帧、贴一次、读回像素,Windows E2E 跑它。

## 更新

设置窗按渠道查 GitHub Releases:稳定渠道读 `releases/latest`,测试渠道读
最近 20 条并取首个比当前新的(含预发布)。资产约定 `lueying-<version>-setup.exe`
+ `.sha256`;更新器复制到 `%TEMP%` 运行,等设置进程退出后静默装包。
发布目录的 `version.txt` 是版本权威。代码签名:CI 配好证书 secret 后自动签
setup.exe,未配置时发布未签名包。

安装包的完整性链分两层:`.sha256` 只防损坏;Release 里带
`lueying-manifest.json` + `.sig`(Ed25519,客户端内置公钥)时,镜像、大小与
哈希以验签后的清单为准,更新器直接拿清单里的哈希,不再单独拉 `.sha256`。
清单在但验不过属于硬错误,更新就地停止;清单缺失退回 `.sha256` 流程。发布
私钥是 64 位十六进制的 Ed25519 种子,存 GitHub secret,由
`cargo xtask update-manifest` + `sign-manifest` 在发布工作流里生成并签名。

## 质量门禁

- `ci.yml`:fmt 回推、clippy `-D warnings`、rustdoc 断链检查、protocol/extjs
  单元测试、快速 E2E + 共享语料基准、server 单元测试、Windows 全链路 E2E
  (TSF 真实链接,x86 定时跑)。
- `quality.yml`(定时):全量语料准确率门限(clean 92% / top-5 95%)、
  x86 全链路、Inno 安装/卸载冒烟。
- `audit.yml`(定时):`cargo audit` 依赖漏洞审计。
- E2E 是主路径:`crates/e2e`(引擎场景 + 基准)、`crates/win-e2e`
  (真实 TSF 宿主)、`crates/install-e2e`(真实安装包)。单元测试补回归网密度,
  覆盖配置解析、清单/补丁校验与页面模型等纯逻辑边界,放在 `src/*_tests.rs`,
  由 `engine`(protocol/extjs)与 `quality`(server)分别执行。
