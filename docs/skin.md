# 皮肤

`%APPDATA%\lueying\skin.toml` 用来覆盖候选窗的配色。文件分 `[light]` 与 `[dark]` 两张表，颜色写成 `0xRRGGBB`。没写的键使用主题默认值。

```toml
[light]
surface = 0xffffff
accent = 0x0a7d32
selected_bg = 0xe6f4ea
selected_text = 0x0a7d32

[dark]
surface = 0x101418
accent = 0x81c995
selected_bg = 0x1f2b23
selected_text = 0x81c995
```

| 键 | 用途 |
|---|---|
| `bg` | 候选窗外层底色 |
| `surface`、`surface_alt`、`segment` | 面板底色、次级底色、分段底色 |
| `border`、`border_strong` | 普通描边、强调描边 |
| `text`、`text_secondary`、`text_tertiary` | 主文字、序号与拼音、辅助提示 |
| `accent`、`accent_hover`、`accent_text`、`on_accent` | 强调色、悬停色、强调上的文字 |
| `hover` | 列表项悬停底色 |
| `selected_bg`、`selected_text` | 高亮候选的底色与文字 |
| `off_bg`、`knob_off` | 设置开关关闭时的底色与滑块 |
| `success`、`danger`、`danger_soft` | 状态色 |

格式或键名有误时，对应的键会被忽略并记入日志。颜色超出 `0xFFFFFF` 的键同样忽略。高对比度模式下 `skin.toml` 完全不生效。
