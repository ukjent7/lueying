# 场景装饰(scene.toml)

候选窗装饰层:在文字下面或上面画几何、图片、精灵与粒子,用声明式表达式驱动动效。纯数据,没有代码;改完存盘 1 秒内热重载。

## 目录与文件

`%APPDATA%\lueying\scene\scene.toml`(设置 → 候选 → 场景装饰 → 目录 可直达;首次打开会写入示例)。图片、精灵帧等素材放同目录,节点里写相对路径。隐私字段(密码框)与系统高对比度下整层自动停用。

## 结构

```toml
[signals.energy]            # 变量:从信号积分出动画值
from = "typing_rate"        # 信号源
gain = 0.6                  # 每秒逼近差距的比例
decay = "1.5s"              # 无输入时指数衰减到 0 的时间

[[nodes]]
id = "glow"                 # 唯一 id,规则按它寻址
kind = "shape"              # shape / image / sprite / text / particles
shape = "rrect"             # rect / rrect / ellipse / line / path
anchor = "highlight"        # panel / preedit / highlight / candidates / caret / screen,可带 .top/.bottom/.left/.right 边
inset = -5                  # 相对锚的内缩,负值外扩
size = [60.0, 2.0]          # 有值时以锚点为中心
offset = [0.0, 1.0]         # 锚点偏移
radius = 6
fill = "accent"             # 调色板名或 #RRGGBB
opacity = "lerp(0.15, 0.55, energy)"
effect = { kind = "blur", radius = "lerp(2, 10, energy)" }

[[rules]]
on = "select"               # key / commit / select / page / backspace_burst / correction / mode_switch / 自定义
do = [
  { tween = "underline.opacity", to = 1.0, over = "120ms", ease = "out_back" },
]
```

## 契约

- **信号**:`typing_rate`、`candidates`、`page`、`page_count`、`mode_chinese`、`selected`、`idle`。只读,随帧更新,只有几何与计数,没有任何文字内容。
- **事件**:内置七种见 `rules.on`;规则可 `emit` 自定义事件,同帧接队,上限 64。
- **表达式**:`lerp/clamp/min/max/abs/floor/ceil/round/sqrt/rand/smooth`,缓动 `out_quad/out_cubic/out_quint/out_back`,比较结果 0/1。每帧有操作预算,超限整帧降级熔断,不会卡死输入。
- **节点**:两层各最多 64 个;`layer = "above-text"` 画在文字上,默认画在文字下。
- **确定性**:同输入必同输出,官方示例同时是黄金测试的回归素材。

## 工具

- `lueying scene check <文件|目录>`:校验,中文报错带行号。
- `lueying scene schema`:输出 JSON Schema,编辑器补全用。
- `lueying scene replay <脚本> [--scene scene.toml] [--json]`:事件模拟器,不敲键盘把动画推完;`LUEYING_SCENE_RECORD=<路径>` 录下真实会话,录出的文件直接喂给 replay 改。
- 官方示例:`crates/scene/scenes/`(光晕、下划线、状态环、背景轮播、mascot)。

## 常见问题

- **改了没生效?** 检查设置里的「场景装饰」开关;解析失败会打日志并沿用上一个合法版本,修好保存即恢复。
- **怎么只换颜色不动?** 用 [skin.toml](skin.md),场景层是给动效与图形用的。
- **动画卡?** 单帧有表达式与粒子预算,粒子池 256;把 blur 半径的表达式范围调小最先见效。
