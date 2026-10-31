# 皮肤

`skin.toml` 用声明式的颜色表覆盖候选窗配色;要动效与渐变,写 SkSL
用户效果(见 [effects.md](effects.md)),那是皮肤之上的高级层。

## 怎么用

把 `%APPDATA%\lueying\skin.toml` 写成浅/深两张表,缺的键用主题默认:

```toml
[light]
surface = 0xffffff
accent = 0x0a7d32
selected_bg = 0xe6f4ea

[dark]
surface = 0x101418
accent = 0x81c995
```

颜色是 TOML 十六进制整数 `0xRRGGBB`。改完 `lueying ext reload` 或在
设置页保存任意一项即生效,不必重启。没有这个文件就是默认主题。

- 高对比度模式下整份忽略:对比度与读屏是宿主不变量,自定义配色压不过它。
- 未知键、类型不对:整份回落默认,原因写在服务日志。
- 值超出 `0xFFFFFF`:只丢那一个键,其余照常生效,日志记一条。
- 文字对比度不做校验:自定义色配出的可读性由皮肤作者负责;效果遮罩
  只对 SkSL 层兜底,不管纯色。
- 目前只作用于候选窗;设置窗、命令面板与模式徽标跟系统主题。

## 键

键名与内置主题字段一一对应,值都是 `0xRRGGBB`:

| 键 | 含义 |
|---|---|
| `bg` | 窗口底色(面板之外的部分) |
| `surface` | 面板底色,候选窗的主面 |
| `surface_alt` | 次级表面(拼音条) |
| `segment` | 分段控件的选中格 |
| `border` / `border_strong` | 描边;`_strong` 是输入框等强描边 |
| `text` / `text_secondary` / `text_tertiary` | 文字三级层级 |
| `accent` / `accent_hover` / `accent_text` | 主色、悬停与深底上的主色文字 |
| `on_accent` | 主色填充上的文字 |
| `hover` | 悬停底色 |
| `off_bg` / `knob_off` | 开关的轨道与圆点 |
| `success` / `danger` / `danger_soft` | 状态色 |
| `selected_bg` / `selected_text` | 选中候选的底与文字 |

部分键(开关、状态色)属于设置窗的语义,在候选窗里不一定可见;
一并开放是为了让两张表完整描述一套配色。

## 与 SkSL 的分工

- 皮肤管「颜色」,效果管「动效」;两个文件互不替代,可以只用其一。
- 效果的 `u_accent` uniform 来自当前主题色:皮肤改了 `accent`,效果
  拿到的也是新主色,两者叠着走。
- 高对比度下两层都停:皮肤整份忽略,效果槽位回落素色。
