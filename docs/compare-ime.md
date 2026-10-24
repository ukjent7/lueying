# 拼音输入法方案对比评审

对比对象:本项目(Ours)、青简(qingjian)、清风(WindInput)、水杉C++(msime-win)、水杉Rust(msime)。路径均相对各自仓库根。

## 1. 进程架构与 IPC

我们:TSF 薄壳 DLL + 常驻 server(crates/server/src/main.rs),命名管道行 JSON(crates/protocol/src/lib.rs),多实例每连接一线程(crates/server/src/pipe.rs);TestKeyDown 预演缓存、OnKeyDown 应用(crates/tsf/src/service.rs:210)。
青简同构但生产细节全:管道 SDDL 放行 Everyone/ALL APPLICATION PACKAGES/低完整性(apps/windows/server/src/ipc/pipe.rs:38),`FILE_FLAG_FIRST_PIPE_INSTANCE` 防双 server;DLL 用 ShellExecuteW 自拉 server(apps/windows/tsf/src/com/service/launch.rs:5)。水杉C++是共享内存+命名管道双通道+能力协商(windows/src/IPC/Ipc.cpp:13)。清风为版本化二进制协议带 barrier(wind_tsf/include/BinaryProtocol.h:9)。
**判定:持平偏落后。** 骨架与微信五家同构,但缺 SDDL(任务栏搜索/设置等 AppContainer 连不上)、无双实例守护、DLL 不自启 server(装完不可用)、行 JSON 无版本协商。

## 2. 引擎

我们:DFS 全切分枚举(上限 64 条,crates/engine/src/segment.rs:19)+ 跨切分合并;整句是"最长匹配贪心路径"(crates/engine/src/query.rs:228),非全局最优;打分 `ln(词频)+0.5×字数+3×ln(1+用户频)`;模糊音 6 组(crates/dict/src/syllables.rs:87);用户词典带周级衰减(crates/engine/src/user.rs:8)但**无持久化调用点**(export_user 无人调)。
青简:维特比整句 + user_ngram 上下文插值(crates/qingjian-core/src/sentence/{viterbi.rs,user_ngram.rs,context.rs})。水杉C++:词图 + kenlm 束搜 beam=32 + 重排(engine/quanpin/word_lattice.h:16,102、lattice_rerank.cpp)。水杉Rust:decode 体系含**按用户接受度折扣的错音纠错**(crates/engine/src/quanpin/typo_edges.rs:1)。清风:DAG+格子+八股文 LM+频次重排(wind_input/crates/wind-engine/src/pinyin/{dag.rs,lattice.rs,octagram.rs}、freq_rerank.rs),调优注释极细(虚词读音判据,lattice.rs:22)。
**判定:落后,差距最大项。** 无二元模型、无全局解码、无纠错、无学习落盘;性能护栏(签名索引+8000 访问上限+档位截断,query.rs:42)是亮点。

## 3. 词典

我们:白霜五文件转 TSV(xtask/src/main.rs:57),启动全量文本解析排序(crates/dict/src/lib.rs:49),无二进制缓存。
清风同源白霜但管线 4962 行:赋权/一元频率/顺序报告/上游序审计/简码(wind_input/apps/wind-tools/src/bin/gen_dict/{weight.rs,order_report.rs,upstream_order.rs}、gen_unigram.rs),且明确"多库取 max 不累加"口径。水杉Rust用 SQLite+mmap(crates/engine/src/dictionary/pinyin.rs:732)。微信:预编译分文件+md5 清单热更新、简拼独立物化(docs/wetype-design-notes.md)。
**判定:数据源持平,格式与质量工程落后。** 启动秒级文本解析不可接受;我们有 12 探针+坏行计数(crates/e2e/src/scenarios.rs:382)算起步版质量门。

## 4. TSF 集成

我们:异步编辑会话(同步会话崩沉浸式应用,crates/tsf/src/edit.rs:4)、组句终止回调、线程焦点清组合、显示属性、Caps/Shift 单击判定(crates/tsf/src/keys.rs:83)、8 类别注册含 IMMERSIVESUPPORT(crates/tsf/src/registry.rs:22)。
三家参考共同有而我们全缺:**GUID_COMPARTMENT_KEYBOARD_DISABLED 密码框检查**(青简 apps/windows/tsf/src/com/context.rs:16、水杉C++ windows/src/Key/KeyEventSink.cpp:727)、语言栏按钮(青简 com/mode/button.rs、水杉C++ windows/src/LanguageBar/)、保留键(水杉C++ CompositionProcessorEngine_PreservedKeys.cpp)、ITfUIElement 候选(水杉C++ windows/src/Candidate/TipCandidateList.cpp)、真实图标(我们传空 icon,registry.rs:70)。键映射 US 布局硬编码(crates/tsf/src/keys.rs:33)。
**判定:落后。** 密码框里继续组句是可感知缺陷。

## 5. 候选窗与 UI

我们:gpui(跟 zed main git 依赖)无焦点置顶窗,空闲停出屏幕防闪烁(crates/server/src/ui.rs:31);光标贴靠+工作区钳制+上方翻转;DPI 经 scale_factor。清风/水杉C++为 Direct2D 自绘,青简含深浅双主题(apps/windows/server/src/ui/candidates/theme/mod.rs:67)、鼠标点选(click.rs)、多屏(monitor.rs)、状态条(status/)。
**判定:落后。** 固定亮色、无鼠标交互、无翻页按钮;gpui 依赖 zed main 是维护风险也是面积优势。

## 6. 设置与持久化

我们:Config::default() 硬编码(crates/engine/src/lib.rs:63),无设置界面、无配置文件、用户词典不落盘。
青简设置 9 页(apps/windows/settings/src/panel/pages/);清风 TOML 四层合并+schema 守门+宿主兼容表(wind_input/crates/wind-config/src/);水杉Rust偏好带值域校验(crates/input-runtime/src/lib.rs)。
**判定:全面落后,空白项。**

## 7. 安装/升级/发布

我们:无安装器、无签名、无发布矩阵,CI 只构建。青简 Inno+签名脚本(apps/windows/installer/{qingjian.iss,release-sign});水杉C++安装器带 VC redist 检测与 x64compatible 声明(installer/msime_setup.iss:76,215);微信五架构+arm64ec+渠道 ini。
**判定:落后,交付链路为零。**

## 8. 工程质量

我们领先项如实列:CI 三门禁 clippy/rustdoc 均 -D warnings + fmt 自动回推(.github/workflows/ci.yml:39);E2E 为唯一验证手段:26 场景+12 探针+性能阈值(均 5ms/最大 10ms)+双词典矩阵工件(crates/e2e/);Windows 全链路 SendInput E2E(crates/win-e2e/src/main.rs);死键盘兜底(server 挂即放行,service.rs:229)+编辑会话 catch_unwind(edit.rs:57);锁文件钉 zed rev 防抖动(ci.yml:20);~4k 行实现全链路。
水杉Rust测试密度更高(lattice/ 下 40+ 测试文件、fuzz/、codeql+audit workflows);青简有 audit.yml。
**判定:领先(对清风/水杉C++),与水杉Rust/青简并驾。**

## 9. 多场景适配

水杉C++最深:游戏兼容白名单+Valve 特判+独占全屏隐藏候选(windows/src/IME/MetasequoiaIME.cpp:778、Global/GameCompat.h、docs/installation.md:71)、TF_TMF_UIELEMENTENABLEDONLY 宿主(CandidateListUIPresenter.cpp:1279);微信笔记印证 arm64ec 与崩溃双层(wetype-design-notes.md)。我们仅声明了 IMMERSIVESUPPORT 类别,无 UiLess/全屏/UAC 专项处理。
**判定:落后。**

## 行动建议(按用户可感知×成本排序)

1. 密码框放行:Activate/每键查 KEYBOARD_DISABLED+EMPTYCONTEXT compartment——极低成本,安全可感知。
2. DLL 自拉 server(ShellExecuteW,仿青简 launch.rs)+管道首实例守护——否则装完不可用。
3. 用户词典落盘:server 定时/退出 export_user 到 exe 目录,启动 import——学习丢失高感知。
4. 安装器(Inno)+真实图标:icon 传 vec![0u16] 占位,任务栏观感差。
5. 管道 SDDL 加 AC/低完整性——修系统应用打不了字。
6. 词典二进制缓存(xtask 增物化产物,mmap 加载)——启动从秒级到毫秒。
7. 候选窗深色主题+鼠标点选+翻页按钮(仿青简 theme/click)。
8. 整句改束搜维特比+bigram(词频表可先从白霜词内共生成为八股文式 LM)。
9. 错音纠错边(仿 msime typo_edges 预算化查表)。
10. UiLess/游戏宿主适配(候选窗全屏隐藏)+arm64/arm64ec 发布矩阵。
