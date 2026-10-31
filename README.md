# 掠影 (Lueying)

<p align="left">
  <img src="https://img.shields.io/badge/Platform-Windows%2010%20(1809+)%20%7C%2011%20(x64)-blue.svg" alt="Platform" />
  <img src="https://img.shields.io/badge/Language-Rust-orange.svg" alt="Rust" />
  <img src="https://img.shields.io/badge/Architecture-TSF%20Multi--Process-informational.svg" alt="TSF" />
  <img src="https://img.shields.io/badge/License-GPL--3.0--or--later-green.svg" alt="License" />
</p>

掠影（Lueying）是一款专为 Windows 平台打造的现代中文拼音输入法，基于 Text Services Framework (TSF) 与 Rust 构建。项目追求极致的输入跟手度、高准确率与纯净安全，提供先进的无焦点 GPU 渲染候选窗与模块化的扩展生态。

---

## 核心特性

- **极致准确的解码与排序**
  - **全拼与长句分词**：基于音节图构建与词图 Viterbi 算法，支持复杂切分并存与末音节自适应补全。
  - **多层排序模型**：融合白霜（rime-frost）词典词频、N-gram 语法模型、4M 轻量模型实时同步重排与 25M 停顿异步重排。
  - **个性化自学习**：本地记录选词提频、同键负分抑制、自动造词与上下文搭配学习。

- **现代化自适应渲染管线**
  - **GPU 渲染优先**：基于 Skia 图形引擎，通过 DirectComposition 交换链与 Ganesh D3D12 后端呈现高帧率无焦点窗口。
  - **CPU 自动无感兜底**：在无硬件加速（如虚拟机、远程桌面）或显卡驱动重启时，自动换手至分层窗口（`WS_EX_LAYERED`）CPU 光栅化，打字不中断。
  - **完备的无障碍体验**：完整适配 Windows 高对比度模式、随系统「减少动态效果」平抑动画，并原生暴露 UI Automation 控件树。

- **分层与沙箱化扩展体系**
  - **输入方案与辅助码**：内置小鹤、自然码、微软、搜狗主流双拼方案，支持五笔单字辅助码过滤。
  - **运行期脚本沙箱**：基于独立的 QuickJS 沙箱线程，支持使用 JavaScript 编写动态外观装饰（SkSL 着色器）与功能命令。
  - **受控平台原语**：提供截图、离线 OCR（系统 OCR 或 PP-OCRv6）、剪贴板、私有存储与系统通知等安全原语，外联网络强制域名白名单。

- **纯粹与本地优先的安全隐私**
  - **零遥测、零云端**：核心输入法不引入网络模块，按键流、候选词与用户词库 100% 留存本机。
  - **敏感场景自动熔断**：在系统密码框、凭据输入与安全桌面下自动直通，停用候选、学习与扩展层。

---

## 系统拓扑

```
lueying_tsf.dll (轻量薄壳，注入宿主进程，仅处理按键协议转发)
    │  命名管道 \\.\pipe\lueying-v1
    ▼
lueying-server.exe (用户常驻服务)
    ├─ 核心解码引擎 (共享二进制词典、语法搭配与重排模型)
    ├─ 候选窗口 (Skia: DirectComposition + D3D12，CPU 兜底)
    ├─ 状态徽标 (分层窗口 CPU 光栅化)
    └─ QuickJS 扩展沙箱 (独立工作线程，隔离外部命令与外观脚本)
lueying-settings.exe (独立设置中心：原生窗口 + Skia 光栅化)
lueying-update.exe (独立更新器：Ed25519 签名验签与静默升级)
lueying-netproxy.exe (扩展网络代理：按需启动，严格限制白名单域名)
```

---

## 快速上手

### 系统要求

- **操作系统**：Windows 10（1809 及以上）或 Windows 11
- **架构支持**：x64（64 位，含 32 位应用兼容支持）

### 安装与启用

1. 前往 [Releases](https://github.com/ukjent7/lueying/releases) 下载最新的 `lueying-<版本>-setup.exe`。
2. 运行安装程序完成安装并注册 TSF 文本服务。
3. 按下快捷键 <kbd>Win</kbd> + <kbd>Space</kbd>，在输入法列表中切换至 **掠影**。

### 常用快捷键

| 操作 | 快捷键 | 说明 |
|---|---|---|
| **上屏首选** | <kbd>Space</kbd> | 确认当前首选候选词上屏 |
| **选择候选** | <kbd>1</kbd> ～ <kbd>9</kbd> | 上屏对应序号的候选词 |
| **候选翻页** | <kbd>-</kbd> / <kbd>=</kbd> 或 <kbd>PageUp</kbd> / <kbd>PageDown</kbd> | 候选词列表向前 / 向后翻页 |
| **切换高亮项** | <kbd>↑</kbd> / <kbd>↓</kbd> | 循环高亮候选词 |
| **原样上屏英文字母** | <kbd>Enter</kbd> | 提交当前原始输入英文字符串 |
| **按音节回删** | <kbd>Ctrl</kbd> + <kbd>Backspace</kbd> | 整体删除光标前的一个音节 |
| **按音节移动光标** | <kbd>Ctrl</kbd> + <kbd>←</kbd> / <kbd>→</kbd> | 在拼音串的音节间左右步进 |
| **中/英文切换** | 单击 <kbd>Shift</kbd> | 切换中英文输入状态 |
| **删除自造词** | <kbd>Shift</kbd> + <kbd>Delete</kbd> | 从用户词库中移除当前高亮词 |

---

## 配置与定制

掠影的所有配置统一保存在 `%APPDATA%\lueying\config.toml` 中，修改即刻热重载生效：

```toml
# 核心输入偏好
scheme = "full"            # 输入方案: "full", "xiaohe", "ziranma", "mspy", "sogou"
aux_code = false           # 是否开启五笔辅助码过滤 (拼音后键入 `)
predict = true             # 是否启用下一词智能联想
fuzzy = "auto"             # 模糊音: "auto" (自动容错), "off"
chinese_punct = true       # 中文全角标点

# 候选外观
page_size = 5              # 每页候选数量: 3, 5, 9
ui_layout = "horizontal"   # 候选排列: "horizontal" (横排), "vertical" (竖排)
ui_font = "standard"       # 候选字号: "small", "standard", "large"
ui_accent = 0              # 主题强调色下标: 0~5 (或 "蓝", "绿" 等)
effects = true             # 是否启用 SkSL 用户效果装饰

# 启用的运行期扩展
extensions = ["shot-ocr"]
```

更丰富的个性化定制包括：
- **界面配色**：编辑 [`%APPDATA%\lueying\skin.toml`](docs/skin.md) 自定义深浅色面板底色与文字颜色。
- **着色器动效**：在 [`%APPDATA%\lueying\effects\`](docs/effects.md) 编写 SkSL 自定义背景与选中块动态着色。
- **自定义短语与标点**：在图形设置中维护 `phrases.txt` 与 `punct.txt`。

---

## 文档导航

### 用户指南
- [安装与系统要求](docs/install.md)：安装流程、版本化平滑升级与数据目录速查。
- [使用指南](docs/usage.md)：基础操作、`v` 模式、`u` 模式与特殊符号输入。
- [设置中心与配置速查](docs/settings.md)：图形设置功能、`config.toml` 字段全解与按应用隔离。
- [界面皮肤定制](docs/skin.md)：声明式十六进制色彩定义与字段说明。
- [用户效果 (SkSL)](docs/effects.md)：着色器槽位契约、Uniform 规范与对比度保护。
- [故障排查与诊断](docs/troubleshooting.md)：常见问题解决、日志跟踪与问题反馈规范。

### 架构与开发
- [技术文档首页](docs/README.md)：掠影技术文档总览与索引。
- [系统架构规格](docs/architecture.md)：进程拓扑、内存模型、渲染换手与质量指标门禁。
- [候选评测与基准](docs/benchmark.md)：冻结共享语料集评测方法、跨引擎对比与延迟门限。
- [项目演进路线图](docs/roadmap.md)：已落地特性全景与未来规划。
- [版本发布工程](docs/release.md)：Tag 驱动的 CI 自动化构建、数字签名与回滚机制。

### 扩展生态开发
- [扩展 SDK 概览](docs/extensions/README.md)：扩展分层矩阵、Rust 扩展契约与最小示例。
- [扩展分层与权限模型](docs/extensions/layers.md)：沙箱资源限额、声明式清单与调用审计机制。
- [运行期脚本扩展规范](docs/extensions/script.md)：JavaScript 扩展开发、宿主原语 API 与指标监控。
- [Coding Agent 开发指南](docs/extensions/agent-guide.md)：面向 AI Agent 与 Linux 无头环境的 CLI 工具链。
- [扩展开发 Skill 模板](docs/extensions/SKILL.md)：符合规范的扩展开发模版与交付清单。
- [外部启动器集成](docs/extensions/powertoys.md)：与 PowerToys Command Palette 等启动器联动。

---

## 参与贡献

欢迎参与掠影的开发与完善！请查阅 [参与贡献指南](CONTRIBUTING.md) 了解环境搭建、静态检查命令及 PR 提交流程。

- **代码准则**：核心算法保持跨平台纯粹，平台依赖集中于 FFI 边界；遵循 Rust 最新稳定版规范。
- **质量门禁**：所有功能均具备针对性的单元测试与全链路 E2E 场景断言，由 GitHub Actions 强制把关。

---

## 安全策略

掠影输入法高度重视用户隐私与系统级输入安全。若发现任何安全漏洞或隐患，请勿创建公开 Issue，请遵循 [安全策略与漏洞披露](SECURITY.md) 提交私密安全报告。

---

## 许可证

掠影核心源代码依据 [GNU General Public License v3.0 or later (GPL-3.0-or-later)](LICENSE) 许可协议开源。项目内置或引用的词典数据、模型权重与字体资产分别遵循各自主体的开源授权协议。
