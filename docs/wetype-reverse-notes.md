# WeType(微信输入法)3.0.0.38 静态逆向笔记

逆向方式:7z 解包官方安装包 → idalib(IDA 9.4 无头 Python SDK)对 5 个关键 PE 做
auto-analysis + 导入表/字符串/调用点/Hex-Rays 反编译提取;辅以全量字符串转储(ASCII+UTF-16)。
仅提取设计与工程事实,不复制任何代码。

- 安装包:`WeTypeSetup_official_p.exe`(NSIS,216MB)
- 解包产物:`/tmp/wetype/extracted/`(主目录 `3.0.0.38/` + 按架构 `WetypeCore_3.0.0.38/<arch>/`)
- idalib 报告:`/tmp/wetype/work/*.report.txt`;字符串转储:`/tmp/wetype/work/str/*.txt`
  (临时目录,关键摘录已内联本文,丢失不影响本文成立)
- 覆盖 PE:`wetype_tip_x64.dll`(薄壳)、`wetype_tip_core.dll`(x64)、`wetype_server.exe`、
  `wetype_renderer.exe`、`wetype_service.exe`;字符串覆盖全部其余 DLL/EXE

## 0. 总纲:WeType 是 mozc(Google 日本输入法开源版)的改造版

这是本次最重要的结论,后续一切架构判断的基础。

证据:
- 全部自研二进制内泄漏构建机源码路径,统一前缀
  `E:\workspace\WeChatPinYin\wxkb\01_mozc\`,其中 `01_mozc` 目录下出现 mozc 开源仓库的同名文件:
  `ipc\ipc_path_manager.cc`、`win32\tip\tip_text_service.cc`、`win32\tip\tip_edit_session.cc`、
  `win32\tip\tip_lang_bar.cc`、`base\system_util.cc` 等
  (来源:server/tip_core/renderer/update 字符串转储,数十处)
- 构建体系为 CMake:`cmake-build-{x86,x64,arm,arm64,arm64ec}-release\...\wetype_tip_x64.pdb` 等 PDB 路径
- protobuf 消息命名空间 `wetype.commands.*` 与 mozc `mozc.commands.*` 一一对应
  (`wetype.commands.KeyEvent/Output/CandidateList/SessionCommand` 等,installer/update 字符串)
- mozc 原生特性原样保留:命名管道 IPC + IPCPathManager、TipTextService 全家、
  `SetLanguageProfileDisplayName failed.`、`GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION`

对我们的含义:研究 WeType 架构 ≈ 研究 mozc 架构,而 mozc 源码公开。
"参考微信输入法"可以落地为"对照 mozc 源码理解它的每一个设计决策"。

## 1. 进程拆分与 IPC

### 1.1 组件职责(证据:文件列表 + 各自字符串/RTTI)

| 组件 | 职责 | 证据摘录 |
|---|---|---|
| `wetype_tip_{x86,x64,arm,arm64,arm64ec}.dll`(~2.4MB) | TSF 薄壳:COM 注册、按架构+版本加载 core | RTTI `TipClassFactory@tsf@win32@wetype`;字符串 `\WetypeCore_`、`\wetype_tip_core.dll`、`Get core version failed.`、`incompatible version`、`CleanGarbageLibraries`;源 `win32\shell\tip_shell_entrance.cc` |
| `WetypeCore_<ver>\<arch>\wetype_tip_core.dll`(~5.4MB) | 宿主进程内真身:TSF 逻辑、键处理、状态推送 | 37 个 `win32\tip\tip_*.cc` 文件名;`wetype::win32::tsf::TipTextServiceBridgeImpl::*` 日志符号 |
| `wetype_server.exe`(7.8MB,x64) | 引擎进程:建管道服务端、加载引擎、词典 | `CreateNamedPipeW` 于 sub_14017D490 + `"CreateNamedPipe failed {}"`;导入 `libwxhld.dll` 的 54 个 `wxime_*` 函数 |
| `wetype_renderer.exe`(7MB,x64) | 候选窗/状态栏渲染进程 | 管道客户端(`tip_client.cc` 反编译见 1.3);`wetype::renderer::ui::FlutterWindow::ShowTopMost` |
| `wetype_service.exe`(2.4MB,x64) | Windows 服务:提权打补丁、装进程 | 服务描述 `"Manages WeType input method."`;RTTI `service@wetype::{TaskBase,ProcessLauncher,PatcherLauncher,PatchCleanup}`;`WETYPE_PATCH_FLOW_ID` |
| `wetype_update.exe`(21.5MB,x64) | 工具栏(TBUI)+ Flutter 宿主(设置/语音/文件传输)+ 全屏检测 + 升级器 | `WetypeUpdate\fullscreen_detector.cc`、`WetypeUpdate\flutterRunner\*` |
| `libwxhld.dll`(19.7MB,x64) | 引擎本体(C ABI) | server 导入表:`wxime_initialize/create_session/process_input/get_candidates/add_cell_dict/config_dict/...` |

引擎 C ABI(server 导入表摘录):`wxime_process_input`、`wxime_get_candidates`、
`wxime_select_candidate`、`wxime_get_jianpin_dict_version`、`wxime_get_second_level_dict_version`、
`wxime_add_cell_dict`、`wxime_cloud_general_search`、`wxime_set_text_around_cursor`(引擎与
server 解耦的稳定边界,引擎可独立换发)。

### 1.2 管道与命名对象(证据:字符串 + 导入表)

- 管道名统一前缀 `\\.\pipe\wetype.` + 动态后缀(installer/server/tip_core/renderer/update
  五处字符串一致);后缀由 mozc 的 `ipc\ipc_path_manager.cc` 逻辑生成,server 反编译
  sub_1401BAD10 中先 `CreateFileW(path, GENERIC_READ, ..., OPEN_EXISTING)` 读取 IPC key 文件
  再组名,错误串 `IPCKey is invalid length` —— 即 mozc 的"key 文件 + 名字哈希"防劫持方案
  (后缀具体构成:未证实,属运行时拼装)。
- server 导入 `GetNamedPipeClientProcessId`:服务端取客户端 PID 识别进程身份
  (是否进一步校验镜像路径:未证实)。
- 事件/互斥:`Local\wetype.event.<x>`、`Local\wetype.mutex.<x>`(installer/server/renderer/tip_core);
  跨用户安装互斥 `Global\com.tencent.wetype.installer`(全部组件都有,用于安装期间静默各进程);
  调试辅助 `Local\WeTypeRefetchDebugHwndV1` / `WeTypeRefetchDebugOwnerMutexV1`。
- 与微信主进程:WM_COPYDATA 转发器(`WetypeUpdate\ipc\wechat_copydata_forwarder.cc`)。
- 共享内存:server/renderer/tip_core 均导入 `CreateFileMappingW/MapViewOfFile`
  (server 内一处固定映射 0x10 字节视图,用途未证实,推测为握手/版本结构)。

### 1.3 客户端连接姿势(renderer 反编译,`win32\tip\tip_client.cc`)

```
CreateFileW(pipe, GENERIC_READ|GENERIC_WRITE, 0, NULL, OPEN_EXISTING,
            0x40190000 /* FILE_FLAG_OVERLAPPED|SECURITY_SQOS_PRESENT|SECURITY_IDENTIFICATION */, NULL)
```
SQOS=IDENTIFICATION 表示客户端**禁止服务端模拟自己**——输入法场景下键盘数据是敏感流,
这是 mozc 沿用至今的安全细节。renderer 同时导入 `WaitNamedPipeW`、`GetNamedPipeServerProcessId`。

### 1.4 协议形态

- protobuf:installer 内嵌描述 `ipc/ipc.proto`、包名 `wetype.ipc`;update.exe 内含全部消息
  描述符(`wetype.commands.*` 数百个,与 mozc commands.proto 同构,如
  `KeyEvent/Output/CandidateWord/CandidateWordSyllable/FuzzyPYInfo/DoublePinSolution`)。
- 消息含 `wetype.commands.CrashInfodump`、`ApplicationInfoprocess`(宿主进程画像)、
  `ClipBoardItemtext` 等 —— 键事件、候选、配置、崩溃、剪贴板全走同一管道协议。

### 1.5 配置/词典如何被各进程读(多通道并存)

1. 版本目录内 `wetype_config.ini`(tip_core/server/renderer/update 都有该文件名字符串;
   `GetPrivateProfileStringW` 导入)——随版本走的机器级配置。
2. MMKV:`\MMKV\` 目录,第三方 `mmkv_1.2.5`(`third_party\mmkv_1.2.5\Core`),
   `base/MMKVUtils.cpp`、`wetype::base::WeTypeMMKVBase`;renderer 侧
   `wetype::renderer::data::MmkvManager::LoadSetupConfig`;MMKV 访问包了 SEH
   (`GetSafeMMKV Error, ExceptionCode={}, LastError={}`)——用户态键值存储(高频设置项)。
3. 注册表:`Software\Tencent\WeType`(全组件)、`Software\Tencent\WeType\Refetch`、
   企业策略 `Software\Policies\WeType Project\WeType\Preferences`(installer/server/renderer/update 均读取)。
4. renderer 另读 `setup.ini`、`renderer_config.ini`(装后首启迁移,有
   `delete renderer_config.ini failed` 迁移痕迹)。

## 2. 词典工程

### 2.1 布局与清单(`3.0.0.38/BuildIn/`)

`index.json`:JSON 数组,28 项,每项字段
`name / md5 / type / uniqueID / version / timestamp / is_cell / in_ime_data / support_update / dict_type`。
即"文件 + md5 + 语义类型 + 独立版本号"的清单驱动加载,词典与主程序完全解耦可独立热更
(`support_update:true`,`in_ime_data:true`)。server 侧还有 `CommonDictIndex.json`、
`HotDictIndex.json` 与日志 `load buildIn dict index file failed`、`parse buildIn dict index file failed`。

按 uniqueID/dict_type 归类的文件(全部位于 BuildIn/):
- 主词库:`bwcnmac`(30MB,dict_type 0)+ `bwcnmacv8`(31MB,v8 新版)
- 简拼:**独立物化** `bwcjpmac_jianpin.bin`(12.6MB,dict_type 9,uniqueID `bwcjp`);
  server 导入 `wxime_get_jianpin_dict_version` 直接查询其版本
- 英文:`bwcnenmac`;语言模型:`lmcnmac`(16MB,LM);二级词典:`slwd_pc_second_level.bin`(51MB,dict_type 7)
- 神经网络:`nlrgpt2_nnlm.bin`(16MB,dict_type 8)+ `tensorflowlite_c.dll`(7.8MB 运行时)
- 五笔:`swbd86/98/swbdnc`(dict_type 3,86/98/新世纪)
- 细胞词库:`cell_dict.sc.zh / .ssrc.zh / .hf.zh`(is_cell:true,dict_type 1)
- 拼音基础设施:`ln2py`(音节→拼音)、`editpinyin`(727KB 拼音编辑/容错表)、`hpmac_base`
- 其他:词性 noun_tag_dict、wsd、prism_v2、pcc、sepd、ckvd 等

### 2.2 二进制格式头部特征(file 偏移 0 处十六进制)

- `editpinyin/lmcnmac/noun_tag_dict` 等以 `01 00 00 80`(LE 0x80000001)+ 递增 u32 偏移表开头
  ——mozc 系 LOUDS/双数组 trie 风格容器(是否 marisa:未证实,无明文字样)
- `cell_dict.ssrc.zh` 头部含明文 `Ime trie header` + 版本计数:自定义 trie 容器
- `cell_dict.sc.zh` 头部为计数直方表(前 24 字节 `00..00 ea 56 fa b3 01 ...`,疑似校验+分段计数)
- 主词库 `bwcnmac.bin` 头部 `e9 75 0a 00`(计数)后跟字节直方图数组

引擎侧证据(libwxhld 字符串):`asso_cell_dict_->IsAssociateKVDict()`(细胞词库按 KV 联想词典
加载)、`UserDictBackUp`/`lookup_user_dict_syllables`(用户词典独立于系统词典)。

### 2.3 降级策略

server 字符串:`safe mode, just load buildin dict`——检测到异常时只加载内置词典的保守模式。

## 3. TSF 集成

注册链(安装时,证据 `WeChatInstallDll.dll` NSIS 插件字符串):
1. 按架构对每个 tip DLL 执行 `regsvr32.exe ...\wetype_tip.dll"`(字符串含
   `wetype_tip_x86/x64/arm/arm64/arm64ec.dll` 与 `\x64\wetype_tip_core.dll` 等架构路径拼接)
2. 薄壳 `DllRegisterServer`(导出表,sub_180011BC0 反编译)写
   `HKCR\CLSID\<clsid>\InProcServer32`:`RegOpenKeyExW(HKEY_CLASSES_ROOT, L"CLSID\\", ...)` →
   `RegCreateKeyExW` → `RegSetValueExW(..., REG_SZ, 值=模块路径)`;同目录字符串 `CLSID\`、`InProcServer32`
3. 安装插件写 `Software\Microsoft\CTF\Assemblies\0x%08x`(按 LANGID 启用 TSF 装配)与
   `SYSTEM\CurrentControlSet\Control\Keyboard Layouts\`
4. CLSID 具体 GUID 值:未证实(二进制内以结构体存在,注册时经 StringFromGUID2 动态格式化)

tip_core 的 TSF 面(源文件名证据):`tip_text_service / tip_edit_session(_impl) /
tip_keyevent_handler / tip_lang_bar(_menu) / tip_ui_element_manager /
tip_ui_element_conventional / tip_compartment_util / tip_private_context / tip_range_util /
tip_status / tip_theme_manager / tip_client`,外加两块自研扩展:
- `tip_imm32_candidate_bridge.cc`:老 IMM32 应用(不认 TSF UIElement)的候选桥接
- `tip_host_context_capture.cc`:抓宿主上下文,日志
  `CaptureHostContext method={} ... channel_ok={} tsf_fallback={} office_fallback={} imr_fail={}`
  ——多级回退(TSF 通道→Office 特判→IMR 请求),为的是跨应用取光标前后文

compartment 事件:`wetype::win32::tsf::TipTextServiceBridgeImpl::InitCompartmentEventSink`,
失败日志 `AdviseCompartmentEventSink GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION failed`
(中英/全半角状态跟随系统输入模式,即语言栏"中/英""全/半"由 compartment 驱动)。

## 4. 候选窗(renderer)

定位分工(导入表证据):
- tip_core 侧:`MonitorFromWindow` x3 —— 由宿主应用窗口选显示器
- renderer 侧:`MonitorFromPoint` + `GetMonitorInfoW` —— 由光标/插入点落点选显示器并取工作区

窗口样式(导入表 + 字符串):
- `CreateWindowExW` / `SetWindowPos` x3 / `GetWindowRect` x3 / `SetLayeredWindowAttributes`(分层窗透明)/
  `DwmSetWindowAttributes`(DWM 暗色标题/圆角)
- 无焦点:`noactivate`、`show_status_bar_without_focus`、`show_toolbar_without_focus`
  (WS_EX_NOACTIVATE,弹窗不偷焦点);置顶:`FlutterWindow::ShowTopMost`、`topmostbtn/notopmostbtn`

深浅色:
- 读 `Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的
  `SystemUsesLightTheme`(tip_core/renderer/update 三处均有)+ `IsThemeActive` + `muxtheme.dll`
- proto 枚举 `wetype.commands.GlobalTypeConfig.ThemeMode`(light/dark)随管道下发

renderer 还导入 `CreateNamedPipeW/ConnectNamedPipe` + `CreateIoCompletionPort` 且有
`server_fatal` 字符串:renderer 不只是纯被动画窗,也有自己的管道端(推测接收 tip_core 的
状态推送 `tip_status_to_renderer_manager.cc`;具体拓扑:部分未证实)。

## 5. 崩溃与升级

崩溃(双通道):
1. CrashRpt v1500(开源库,`Copyright 2003-2013 The CrashRpt Project Authors`,PDB 路径
   `third_party\crashrpt2\`):`CrashRpt1500.dll` 随**每个架构目录**分发(挂在 TIP/core 旁),
   `CrashSender1500.exe` 独立进程上报;事件 `Local\CrashRptEvent_%s(_2)`;
   失败缓存 `%TEMP-like%\CrashRpt\UnsentCrashReports\%s_%s` 离线重发
2. 自托管 Sentry:tip_core/server/renderer 内嵌
   `https://wetype.weixin.qq.com/sentry/api/14/minidump_v2/` 与 `.../envelope/`
   (server 侧 envelope 通道,即 event+dmp 双轨)

升级(证据链完整):
- 版本目录双轨:主目录 `<root>\3.0.0.38\` + core 目录 `<root>\WetypeCore_3.0.0.38\<arch>\`,
  薄壳运行时按"目录名版本 + core 版本握手"加载(`Get core version failed.`、`incompatible version`)
- update.exe 内正则 `\d+\.\d+\.\d+\.\d+\.tmp\d+|WetypeCore_\d+\.\d+\.\d+\.\d+\.tmp\d+`:
  新版本先落 `.tmpN` 目录,校验后原子改名切换(版本目录级原子升级)
- server/tip_core 导入 `MoveFileTransactedW` + ktmw32 `CreateTransaction/CommitTransaction`:
  事务性文件替换,失败自动回滚
- 提权替换:service.exe 的 `PatcherLauncher/PatchCleanup`(运行中被占用的 DLL 交给服务进程替换),
  tip 侧日志 `wt_pc_patch_launch_requested / wt_pc_patch_error_launch_service`
- 旧版本回收:薄壳 `TipTextServiceBridgeImpl::CleanGarbageLibraries`
- 安装完成后降权:`DeElevateStartProcess ShellExecute %s %s`(installer)——提权只用于安装瞬间,
  常驻进程一律中完整性(输入法注入所有应用,绝不能高完整性常驻)
- 版本共存判定:update.exe `IsWeTypeActiveInputProfile: hr=... clsid={} profile={} clsid_match={}
  active={}`——升级前确认 TSF 当前激活的 profile 是否本输入法,避免打扰用户输入

## 6. 四架构布局(PE Machine 字段实测)

| 文件 | Machine | 说明 |
|---|---|---|
| `wetype_tip_x86.dll` | 0x14c x86 | 32 位进程宿主(SysWOW64) |
| `wetype_tip_x64.dll` | 0x8664 x64 | 主流 |
| `wetype_tip_arm.dll` | **0x1c4 ARMNT** | Windows on ARM 32 位 Thumb-2 进程 |
| `wetype_tip_arm64.dll` | 0xaa64 arm64 | ARM64 原生进程 |
| `wetype_tip_arm64ec.dll` | **0x8664(x64 头)** | ARM64EC:PE 头标 AMD64 + ARM64X 重定位,供 x64/ARM64 互操作进程加载 |
| 根目录 `wetype_tip.dll` | 0xaa64 arm64 | 默认注册指向的副本 |

- core 按架构子目录:`WetypeCore_3.0.0.38\{x86,x64,arm,arm64,arm64ec}\wetype_tip_core.dll + CrashRpt1500.dll`
  (arm64ec 目录内的 core PE 头同为 x64 标记,符合 ARM64EC 惯例)
- server/renderer/service/update/libwxhld **仅 x64**:重进程不做多架构,arm64 机器上走 x64 模拟
- WeChatInstallDll.dll 内有全部架构路径拼接串(`\x64\wetype_tip_core.dll`、`\arm\...`、`\arm64\...`、`\arm64ec\...`)
- 结论:多架构成本集中在"进程内组件"(tip 薄壳 + core + CrashRpt),进程外组件单架构 —— 我们
  的发布矩阵照此办理(x64 必做,arm64/arm64ec 薄壳备着,server 不必多架构)

## 7. 多场景证据

- **游戏全屏**(update.exe,工具栏进程):`WetypeUpdate\fullscreen_detector.cc`,
  `FullscreenDetector::InstallHook`(WinEvent 钩子)+ `UpdateFullscreenStateLocked`;
  判定启发式日志 `TBUI fullscreen hit/miss class={} style={} caption={} zoomed={} std_max={}
  covers_work={} intrudes_taskbar={} win/work/monitor rect`(窗口类名+样式+是否盖住工作区+
  是否侵入任务栏);命中后 `fullscreen_suppressed=true` 抑制浮层,事件 `event:fullscreenChanged`
- **UAC/提权**:服务进程补丁流(见 §5)+ installer `runas`/`AdjustTokenPrivileges` +
  `DeElevateStartProcess`;`elevated_process_disabled` 策略项
- **密码框安全模式**:WeType 侧未证实——未见 `TIPCAP_SECUREMODE`/密码相关明文;
  (我们已在 registry.rs 声明 GUID_TFCAT_TIPCAP_SECUREMODE,方向一致,无对账必要)
- **宿主画像**:`wetype.commands.ApplicationInfoprocess`(proto)按进程画像做兼容分叉;
  server 导入 `K32GetModuleFileNameExW/K32GetProcessImageFileNameW`(枚举客户端进程镜像)
- **离线兜底**:`safe mode, just load buildin dict`(§2.3)

## 8. 对我们项目的采纳建议(按价值排序)

我们现状:tsf 薄壳 + 行 JSON 管道 + server(引擎+UI 同进程)+ 文本词典 + 签名简拼索引。

1. **管道安全细节**(成本:小/直接抄配置)
   - client `CreateFileW` 加 `SECURITY_SQOS_PRESENT|SECURITY_IDENTIFICATION`(防 server 模拟客户端);
   - server 侧 `GetNamedPipeClientProcessId` 记录客户端 PID(日志/限权);
   - 管道名加每用户/每会话后缀而非全局固定名 `pinyin-ime-v1`(防跨会话抢注;我们尚无 IPCKey
     握手,可先做"用户 SID 哈希入名",成本小收益立得)。
2. **崩溃双层 + 自托管上报**(成本:中):TIP 侧(宿主进程内)装 CrashRpt 等价物(Rust:
   `windows` crate 的 SEH/vector handler + minidump),server 崩溃时 TSF 直通英文(已有);
   上报先落本地 `.dmp` 目录 + 简单 HTTP POST,对齐其 `UnsentCrashReports` 离线重发语义。
3. **版本目录原子升级**(成本:中):`<install>\app\<ver>\` + `.tmpN` 暂存目录 + 装妥后改名 +
   `MoveFileTransactedW` 替换被占用文件 + 启动时清理旧版本目录(`CleanGarbageLibraries` 等价物);
   GitHub Releases 阶段直接适用,免提权服务(单用户安装于 LocalAppData 时无占用冲突)。
4. **词典清单化 + 独立热更**(成本:中):照 index.json 做 `dict/index.json`(name/md5/版本/
   类型),词典更新与主程序解耦;简拼物化成独立文件暂缓(我们签名索引已够用,
   词库上 50MB 再拆,WeType 用 12.6MB 专门表印证了该阈值判断)。
5. **深浅色 + 无焦点候选窗**(成本:小):读 `Themes\Personalize\SystemUsesLightTheme` 切主题;
   候选窗 `WS_EX_NOACTIVATE | WS_EX_TOPMOST` + layered;定位:TSF 侧传宿主窗口 rect,
   UI 侧 `MonitorFromPoint+GetMonitorInfoW` 兜底(双保险)。
6. **安全模式降级**(成本:小):server 加载词典失败→只载内置最小词典仍可用
   (`safe mode, just load buildin dict` 语义),避免"词典坏了=输入法全死"。
7. **ARM64EC/arm64 薄壳矩阵**(成本:中,远期):多架构只做进程内组件(tip DLL),
   server/renderer 维持 x64 单架构。
8. **不跟**:protobuf 管道(行 JSON 调试优势更大,量级未到)、MMKV(配置量小,注册表/ini 够)、
   Flutter、语音/文件传输/云词库/NNLM/五笔、企业策略键、提权服务(GitHub 发布形态不需要)。

## 附:与既有笔记的差异修正(docs/wetype-design-notes.md)

- 旧笔记"二进制未深加壳,无需 IDA 深挖"——本次已深挖,新增:mozc 血统、IPC key 防劫持、
  protobuf 协议、CrashRpt+Sentry 双轨、`.tmpN`+事务替换升级链、ARMNT/arm64ec 实测、
  全屏检测与宿主上下文捕获细节。
- 旧笔记"arm64ec 列入 M6"维持;补充:arm(ARMNT 0x1c4)在 3.0.0.38 仍随包分发。
