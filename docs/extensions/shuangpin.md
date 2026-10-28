# 双拼

双拼是第一种输入方案扩展。每两个键对应一个音节,展开成规范全拼后交给核心词图;词典、语法模型、模糊音、手误纠正和用户词库全部复用。

## 支持方案

| 配置值 | 名称 | 示例:`你好` | 说明 |
|---|---|---|---|
| `full` | 全拼 | `nihao` | 默认 |
| `xiaohe` | 小鹤双拼 | `nihc` | `v=zh`、`i=ch`、`u=sh` |
| `ziranma` | 自然码 | `nihk` | `k=ao` |
| `mspy` | 微软双拼 | `nihk` | `;=ing` |
| `sogou` | 搜狗双拼 | `nihk` | `;=ing` |

在设置页「输入方案」里选择,或编辑 `%APPDATA%\lueying\config.ini`:

```ini
scheme=xiaohe
```

## 行为

- 组合里显示原始双拼键;候选、内联预览与上屏是汉字。
- 候选的区间映射回双拼键,Backspace 逐键回删,Ctrl+Backspace 按音节删除。
- 学习键统一为规范全拼(`ni'hao`)。用小鹤学过的词,切到全拼或自然码后仍能召回。
- 微软/搜狗的 `;` 只在组合中作为 `ing` 韵母,空闲时仍是普通标点。
- 方案切换时未落定的组合会被清掉,避免旧键按新布局误读。

## 边界

- 双拼每两键一音节;手动隔音符 `'` 会重置配对起点,供混合简拼使用。
- 不支持在双拼组合里逐字母简拼(如 `n'hc` 之外的形式);需要混合输入时用 `'` 明确分段。
- 布局表在 `crates/features/src/shuangpin.rs`。新增布局需要同时更新 `SchemeId`、设置页选项与 E2E。

## 实现位置

- `crates/features/src/shuangpin.rs`:布局、两键展开、双向位置映射。
- `crates/engine/src/scheme.rs`:`Scheme` 契约与默认全拼。
- `crates/engine/src/input.rs`:方案展开、候选区间映射、学习键。
