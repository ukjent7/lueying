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
    ├─ 候选窗 + 模式徽标(无焦点置顶浮层;GPUI 默认,`ui_backend=skia` 走 Skia)
    ├─ UIA 左邻文本回退线程 / 25M 停顿重排线程
    └─ 日志(%LOCALAPPDATA%\lueying\logs\server.log)
lueying-settings.exe(设置窗,经管道与 Server 通信)
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

## 用户数据

| 文件 | 内容 |
|---|---|
| `%APPDATA%\lueying\config.ini` | 全部设置(键=值,未知键跳过) |
| `%APPDATA%\lueying\phrases.txt` | 自定义短语(`短语<TAB>编码`) |
| `%APPDATA%\lueying\punct.txt` | 自定义标点(`键=输出`) |
| `%APPDATA%\lueying\apps.ini` | 按应用覆盖(`[exe]` 节 + `punct`/`learning`) |
| `%LOCALAPPDATA%\lueying\user.tsv` | 用户词库(选词、造词、搭配、置顶) |
| `%LOCALAPPDATA%\lueying\logs\server.log` | 服务日志(超 1MB 滚动) |

设置窗保存 `config.ini` 后发 `ReloadConfig`,Server 重读全部文件并把新配置
下发给所有会话;按应用覆盖在会话上线(`Hello`)时套用。设置里的「配置备份」
把上述文件打包成单份文本(按行数声明块长度),导入时逐块写回、词库走管道合并。

## 候选窗

GPUI 窗口即面板矩形(Windows 上按矩形命中,非 WS_EX_LAYERED);空闲停靠在
虚拟桌面外。排列有横排(自动换行)与竖排;候选字号三档只缩放候选行;主题色
预设只改强调色,高对比度主题始终用系统色。窗口宽度按全部候选估宽,高度按
当前页行数估算,随 DPI 每显示器计算。动效:展开 140ms、上屏淡出 90ms、
翻页 90ms 横向滑动,全部可被按键打断,「减少动态效果」时走终态。

渲染后端可切:`ui_backend=gpui|skia`(config.ini 隐藏键,默认 gpui,改动需重启
服务)。Skia 路线(`ui_skia`)是分阶段迁移的窗口骨架:独立线程的 Win32 无焦点
窗 + DirectComposition 交换链 + Ganesh-D3D12(无 D3D12 硬件退 WARP,建不出
设备时放弃候选窗、输入照常),摆位/DPI/全屏避让与 GPUI 路径共用 `ui_place`;
绘制在平台无关的 `lueying-render`(Linux E2E 光栅像素断言),Windows CI 以
`LUEYING_SKIA_SELFTEST` 自检整条管线。淡出/翻页滑动/鼠标交互还没搬过来,
补齐 parity 前默认保持 GPUI。

## 更新

设置窗按渠道查 GitHub Releases:稳定渠道读 `releases/latest`,测试渠道读
最近 20 条并取首个比当前新的(含预发布)。资产约定 `lueying-<version>-setup.exe`
+ `.sha256`;更新器复制到 `%TEMP%` 运行,等设置进程退出后静默装包。
发布目录的 `version.txt` 是版本权威。代码签名:CI 配好证书 secret 后自动签
setup.exe,未配置时发布未签名包。

## 质量门禁

- `ci.yml`:fmt 回推、clippy `-D warnings`、rustdoc 断链检查、快速 E2E +
  共享语料基准、Windows 全链路 E2E(TSF 真实链接,x86 定时跑)。
- `quality.yml`(定时):全量语料准确率门限(clean 92% / top-5 95%)、
  x86 全链路、Inno 安装/卸载冒烟。
- `audit.yml`(定时):`cargo audit` 依赖漏洞审计。
- E2E 是唯一测试机制:`crates/e2e`(引擎场景 + 基准)、`crates/win-e2e`
  (真实 TSF 宿主)、`crates/install-e2e`(真实安装包)。
