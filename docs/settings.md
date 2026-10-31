# 设置与配置

掠影提供独立的图形化设置窗口（`lueying-settings.exe`），所有配置最终统一持久化于 `%APPDATA%\lueying\config.toml`。设置项保存即生效，无须重启常驻服务。

## 图形设置中心

通过开始菜单「掠影设置」或托盘语言栏菜单启动。

### 核心功能区

| 分类 | 主要配置项 | 说明 |
|---|---|---|
| **输入** | 输入方案 | 支持全拼、小鹤双拼、自然码、微软双拼、搜狗双拼 |
| | 辅助码 | 拼音后按 `` ` `` 接五笔字形码快速过滤单字 |
| | 中文标点 | 中文输入状态下输出全角标点 |
| | Shift 单击切中英 | 开启后单击 Shift 切换状态，长按仍为组合修饰键 |
| | 繁体输出 | 输出繁体中文；提供通用、台湾、香港三种区域字形 |
| **候选** | 实时预览 | 根据当前字号、排列与强调色实时渲染预览效果 |
| | 每页候选数 | 可选 3、5、9 项 |
| | 候选排列 | 横向单行自动折行，或纵向每行一项 |
| | 候选字号 | 小、标准、大（仅缩放候选文字，拼音行保持比例） |
| | 主题色 | 内置预设强调色（高对比度模式下自动跟随系统） |
| | 联想与模糊音 | 控制下一词智能联想与全自动模糊音容错 |
| **扩展** | 官方扩展 | 按需下载管理 PP-OCRv6 离线识别包与 25M 重排模型 |
| | 脚本扩展列表 | 查看、启用、停用已安装的 QuickJS 脚本扩展并审查权限 |
| | 扩展专属设置 | 动态渲染扩展清单声明的配置项（开关、单选、密钥） |
| **词库** | 自定义短语 | 原生文本编辑框维护 `phrases.txt`，保存即生效 |
| | 自定义标点 | 原生文本编辑框维护 `punct.txt` 映射表 |
| | 按应用配置 | 配置特定应用独立的标点输出与词库自学习策略 |
| | 备份与还原 | 一键打包导出用户配置文件与词库，或导入恢复 |
| **关于** | 运行诊断 | 导出环境版本、配置快照与服务端日志片段便于排障 |
| | 更新检测 | 选择稳定或测试更新渠道，并手动检查新版本 |

### 辅助功能与无障碍

- **UI Automation 完整支持**：设置界面完全暴露语义化控件树，为屏幕阅读器（如讲述人、NVDA）提供准确的角色、标签与状态，并支持标准键盘导航（`Tab` / `Shift + Tab` 切换焦点，`Enter` / `Space` 触发，`←` / `→` 调节分段）。
- **原生编辑窗口**：短语与标点编辑使用系统原生控件，读写无需外部文本编辑器。

## 配置文件 `config.toml`

配置文件位于 `%APPDATA%\lueying\config.toml`，采用 TOML 格式。非法键将被忽略并记录警告，单项错误不影响其他配置项加载。

### 常用字段速查

```toml
# 基础输入设置
scheme = "full"                 # 方案: "full", "xiaohe", "ziranma", "mspy", "sogou"
aux_code = false                # 是否开启五笔辅助码
predict = true                  # 是否开启下一词联想
fuzzy = "auto"                  # 模糊音: "auto" (自动容错), "off"
chinese_punct = true            # 是否使用中文全角标点
shift_toggle = true             # 是否允许单击 Shift 切换中英文
traditional = false             # 是否输出繁体中文
traditional_region = "general"  # 繁体区域: "general", "tw", "hk"

# 界面外观设置
page_size = 5                   # 每页候选数量: 3, 5, 9
ui_layout = "horizontal"        # 候选排列: "horizontal" (横排), "vertical" (竖排)
ui_font = "standard"            # 字号档位: "small", "standard", "large"
ui_accent = 0                   # 主题强调色下标: 0~5 或颜色名 (如 "蓝", "绿")
effects = true                  # 是否启用用户 SkSL 装饰效果

# 系统与分发
update_channel = "stable"       # 更新渠道: "stable", "beta"
extensions = ["shot-ocr"]       # 启用的运行期扩展列表

# 扩展配置命名空间
[ext.shot-ocr]
notify = true
quality = "high"
```

> [!TIP]
> 开发者可执行 `lueying-settings.exe --dump-config-schema` 直接导出完整的配置项 JSON Schema，便于工具链与自动化脚本集成。

## 私密配置存储

若扩展清单中声明了 `"type": "secureText"` 类型的设置项（如 API Token），该值不会写入 `config.toml`，而是被持久化在安全隔离的 `%APPDATA%\lueying\secrets\<id>.json` 中：
- 采用当前系统用户专属读写权限（0600 等效）。
- 写入过程先落临时文件后原子重命名替换。
- 运行日志与错误诊断输出中一律抹去密钥内容。

## 自定义标点与按应用配置

### 自定义标点 `punct.txt`
位于 `%APPDATA%\lueying\punct.txt`，格式为一行一条 `键=输出`。键为英文半角符号，输出最长支持 16 字符：
```ini
,=、
^=……
```

### 按应用隔离 `apps.toml`
位于 `%APPDATA%\lueying\apps.toml`。以目标宿主进程可执行文件名（全小写）为小节名，支持单独控制标点与自学习：
```toml
[notepad.exe]
punct = false      # 在记事本中默认使用半角标点
learning = false   # 禁用该应用的打字自学习记录

[code.exe]
punct = false
```
未特别指定的字段将继承全局默认配置。

## 官方离线资产管理

在「扩展 → 官方扩展」中可按需安装大体积离线资产（地址与哈希固定于 `Catalog/official-extensions.json`，经 SHA-256 校验后安装）：
- **PP-OCRv6 离线识别包**：包含检测与识别 ONNX 权重（约 31 MB），纯 Rust 本地推理，无网络通信。
- **25M 神经重排模型**：用于打字停顿期的长句优化（约 25 MB），未安装时保持 4M 轻量模型同步重排。

## 数据备份与迁移

在设置界面的「词库 → 配置备份」中：
- **导出**：将 `config.toml`、`phrases.txt`、`punct.txt`、`apps.toml` 及用户学习词库统一序列化为结构化单文本备份文件。
- **导入**：逐块恢复配置并自动执行词库安全合并，完成后立即触发配置重载。
