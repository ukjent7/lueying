# 系统架构

本文档面向核心开发者，阐明掠影输入法的系统拓扑、核心路径、数据模型与工程约束。具体配置请参考[设置文档](settings.md)，用户操作请参考[使用指南](usage.md)。

## 进程模型与拓扑

掠影采用多进程隔离架构，以保证宿主应用的稳定性与输入法的安全性：

```
lueying_tsf.dll（轻量薄壳，注入宿主进程）
    │  命名管道 \\.\pipe\lueying-v2（长度前缀 postcard 帧，含用户哈希与会话 ID）
    ▼
lueying-server.exe（用户常驻服务）
    ├─ 引擎会话（按宿主线程隔离，共享词典、模型与用户词库）
    ├─ 候选窗口（Direct2D/DirectWrite：DirectComposition 交换链 + WARP 兜底）
    ├─ 状态徽标（原生分层窗口：Direct2D 预乘位图 + UpdateLayeredWindow）
    ├─ 后台工作线程（UIA 上下文获取、停顿重排等）
    └─ 本地日志（%LOCALAPPDATA%\lueying\logs\server.log）
lueying-settings.exe（独立设置窗口：Slint + AccessKit 无障碍，管道通信；更新从这里下载安装包）
lueying-user.exe（安装辅助工具：管理用户态输入法注册表与自启）
```

### 隔离原则

- **宿主纯净**：`lueying_tsf.dll` 仅依赖 `lueying-protocol`，不引入图形库与复杂引擎依赖。按键消息与编码转换在服务端边界完成。
- **无网络依赖常驻**：常驻服务端 `lueying-server.exe` 独立编译且不链接 `winhttp.dll`（由 CI 进行导入表断言）。
- **网络能力隔离**：联网只发生在设置窗（更新检查与扩展安装）与独立
  broker 进程 `lueying-broker.exe`；扩展的 `network` 服务按清单声明的
  域名白名单逐请求强制，broker 兜传输限额的底，常驻服务端
  `lueying-server.exe` 保持离线（CI 导入表断言）。

## 核心引擎

输入引擎由音节图构建与词图 Viterbi 解码构成：

1. **切分与候选生成**：支持同一输入的多种切分并存（束宽为 7）。
2. **多层排序流水线**：
   - 基础词典词频权重。
   - N-gram 语法搭配得分（白霜语法模型）。
   - 用户词库个性化加分与负分机制。
   - 上下文字级搭配（`@context`）与词级转移（`@transition`）。
   - **8M 轻量模型异步重排**：结合左邻上下文在后台单槽 worker 计算，按键路径只投递不等待；结果落地前校验上下文序号与候选版本，避免旧结果顶回新输入。
   - **25M 模型异步重排**：打字停顿达 120ms 时触发异步重排建议。
3. **容错机制**：手误纠正在无法生成有效整句候选时运行，执行预算严格限制在 6ms 内。
4. **数据物化**：基于白霜（rime-frost）词典生成 mmap 内存映射二进制格式（`main.pyd`），损坏时优雅降级为 TSV 词库，再降级为内置微型词典。

## 渲染管线

Windows 侧的输入法表面共用同一套 Direct2D/DirectWrite 绘制代码，遵循 **硬件优先、软件设备兜底** 原则：

- **候选窗口**：独立无焦点 Win32 窗口（`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`），D3D11 → Direct2D → DirectComposition 交换链呈现；无硬件适配器时用 WARP 软件设备，绘制代码不变。
- **异常恢复**：设备被摘除或驱动重启时，就地重建交换链与设备上下文，保证打字过程无中断。
- **状态徽标与面板**：同一绘制层画到 32 位预乘位图，再经分层窗口或 BitBlt 上屏；徽标不占 GPU 资源，并在布局上与候选窗口动态避让。
- **背板材质**：候选窗底色可交给 DWM accent 亚克力，自绘层只铺半透明内容，圆角由 DWM 裁；系统关闭「透明效果」、高对比度、用户皮肤或调用失败时回自绘实底。
- **动效规格**：出现走弹簧（自落点下方 6pt 弹回，约 260ms）、淡出 90ms、翻页 90ms 滑动；支持随时被按键打断；系统开启「减少动态效果」时直接呈现终态。

macOS 侧的候选窗是 InputMethodKit 外壳：无边框 `NSPanel` + 自绘 `NSView`，底色交给系统材质背板——macOS 26 用液态玻璃 `NSGlassEffectView`，更早的版本退回 `NSVisualEffectView`；系统要求降低透明度或提高对比度时改用自绘实底。出现动效由阻尼弹簧驱动：窗口自下方 6pt 弹回原位，约 260ms 落定；系统开启「减少动态效果」时直接呈现终态。布局、命中与绘制指令仍与 Windows 侧共用 `lueying-render` 的同一份几何。

## 用户数据模型

输入法全部用户数据统一存放在标准用户目录，遵循最小特权与原子操作：

| 路径 | 内容 | 说明 |
|---|---|---|
| `%APPDATA%\lueying\config.toml` | 主配置文件 | TOML 格式；忽略未知键，非法值记录日志并告警 |
| `%APPDATA%\lueying\phrases.txt` | 自定义短语 | 格式：`短语<TAB>编码[<TAB>权重]` |
| `%APPDATA%\lueying\punct.txt` | 自定义标点 | 格式：`键=输出` |
| `%APPDATA%\lueying\apps.toml` | 应用隔离配置 | 针对特定可执行文件的标点与学习规则 |
| `%APPDATA%\lueying\secrets\<id>.json` | 扩展私密配置 | 仅限当前用户权限，支持原子写入 |
| `%APPDATA%\lueying\models\` | 本地模型目录 | 按需下载的 25M 重排模型 |
| `%APPDATA%\lueying\extensions\` | 原生扩展目录 | 按需安装的 PP-OCRv6 识别引擎(独立进程)与 `consent.json` 同意记录 |
| `%LOCALAPPDATA%\lueying\user.tsv` | 用户个性化词库 | 记录选词历史、造词、搭配与置顶数据 |
| `%LOCALAPPDATA%\lueying\logs\server.log` | 服务端日志 | 超过 1MB 自动滚动，只保留单份归档 |

设置窗口保存配置后通过管道向 Server 发送 `ReloadConfig` 信号，各引擎会话立即热重载生效。

## 扩展机制

掠影采用清晰的扩展契约与信任边界：

- **编译期契约（Rust）**：
  - `Scheme`：负责击键到音节的规范化映射（如双拼、五笔辅助码）。
  - `Module`：在候选生成与上屏环节对结果进行过滤、重排或改写（如自定义短语、联想）。
- **运行期扩展（Extism JIT 沙箱）**：
  - 每个扩展一个 worker 线程，`lueying-extmeta` 管理清单与权限，命令与外观在 worker 上同步执行。
  - 外观扩展仅可访问场景状态并返回主题色补丁，绝不派发候选汉字、按键历史或应用名称。
  - 场景皮肤扩展（kind=scene）是纯声明包：包内 scene.toml 与素材由候选窗场景层直接加载并热重载，启用列表里第一个皮肤生效；没有可执行体，不进 Wasm 运行时。
  - 线性内存 16 MiB、值栈 256 KiB、燃料计量（初始化 50M / 命令 200M / 外观 20M）；trap 后实例仍可用，连续 3 次故障自动隔离停用。
  - 截图、识别、面板等服务是阻塞调用，只阻塞扩展 worker；扩展卸载时在 50ms 内取消。

## 升级与分发

- **版本权威**：发布产物以安装目录的 `version.txt` 为准。
- **完整性与签名**：
  - 安装包提供 `.sha256` 校验。
  - 发布版本附带 `lueying-manifest.json` 及其 Ed25519 数字签名，设置窗验证签名后才采纳镜像与哈希。
  - Windows 安装包在发布流程中自动注入 Authenticode 数字签名。
- **平滑升级**：TSF DLL 文件名携带版本后缀，避免覆盖冲突；设置窗把安装包下载到 `%TEMP%` 校验后直接运行。

## 质量保障体系

- **持续集成（CI）**：
  - 静态检查：`cargo fmt --check`、`cargo clippy -- -D warnings`、rustdoc 链接检查。
  - 单元与集成测试：协议序列化、扩展沙箱、配置边界测试。
  - 真实全链路 E2E：在 Windows 运行环境中验证真实的 TSF 注入、宿主交互与安装包生命周期。
- **指标与基线控制**：
  - **二进制体积**：全套运行时二进制总和 ≤ 90 MiB。
  - **内存占用**：服务端常驻峰值工作集 ≤ 128 MB。
  - **交互延迟**：按键响应 p95 ≤ 60ms，提交上屏 p95 ≤ 60ms。
  - **准确率门限**：基于固定测试语料，确保日常用例 top-1 准确率高于门限。
