# 界面皮肤

掠影通过 `%APPDATA%\lueying\skin.toml` 提供声明式色彩定义，允许用户精确覆盖候选窗口在深浅色模式下的各项配色。若需要动态渐变或着色器动效，请参考[用户效果文档](effects.md)。

## 配置文件规范

在 `%APPDATA%\lueying\skin.toml` 中定义 `[light]` 与 `[dark]` 两张色值表。色值采用 TOML 十六进制整数格式（`0xRRGGBB`）：

```toml
[light]
surface = 0xffffff          # 候选窗口主面板底色
accent = 0x0a7d32           # 主题强调色
selected_bg = 0xe6f4ea      # 当前选中候选的背景色
selected_text = 0x0a7d32    # 当前选中候选的文字颜色

[dark]
surface = 0x101418
accent = 0x81c995
selected_bg = 0x1f2b23
selected_text = 0x81c995
```

修改文件后，在图形设置页保存任意项或运行 `lueying ext reload` 即可热生效，无须重启服务。

## 配色键定义

所有键值均对应内置渲染主题字段，未特别指定的键自动沿用默认主题配色：

| 字段键名 | 含义说明 | 适用场景 |
|---|---|---|
| `bg` | 候选窗口外层底色 | 窗口外边距与极简边框 |
| `surface` | 面板主底色 | 候选词列表核心展示区 |
| `surface_alt` | 次级面板底色 | 拼音编码行与辅助信息区 |
| `border` / `border_strong` | 基础描边 / 强调描边 | 窗口外框与输入控件分界线 |
| `text` | 主文字颜色 | 候选词汉字文本 |
| `text_secondary` | 次级文字颜色 | 候选序号（1~9）与拼音字母 |
| `text_tertiary` | 弱化文字颜色 | 注音、辅助码提示与快捷指示 |
| `accent` | 主题强调色 | 首选指示器与强调重点 |
| `accent_hover` | 悬停强调色 | 鼠标悬停时的激活反馈色 |
| `on_accent` | 强调色上的反差文本 | 高亮实底上的反白文字 |
| `selected_bg` | 选中高亮项底色 | 当前高亮候选词的背景块 |
| `selected_text` | 选中高亮项文本色 | 当前高亮候选词的文本渲染色 |
| `hover` | 基础悬停色 | 列表项鼠标悬浮背景 |

## 约束与分工

- **无障碍优先原则**：当系统启用「高对比度模式」时，自定义 `skin.toml` 会被整份忽略，色彩渲染严格交由 Windows 系统无障碍令牌接管。
- **容错降级**：出现无法识别的键名或格式错误时，输入法回退到默认主题并将原因记录在服务日志中；单键值超出 `0xFFFFFF` 时仅丢弃该键。
- **与 SkSL 动效的分工**：
  - `skin.toml` 负责**基础颜色体系**的静态定义。
  - SkSL 效果负责**动态着色与粒子动效**；效果着色器接收的 `u_accent` Uniform 会自动与当前皮肤定义的 `accent` 同步。
