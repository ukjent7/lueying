# 设置

从开始菜单的「掠影设置」或语言栏菜单打开。设置写入 `%APPDATA%\lueying\config.ini`,保存后立即生效,不需要重启输入法。

| 分组 | 设置 | 说明 |
|---|---|---|
| 输入 | 输入方案 | 全拼、小鹤、自然码、微软、搜狗双拼,共享同一份词库与学习记录 |
| 输入 | 辅助码 | 拼音后按 `` ` `` 再打 1–2 位五笔码,用来在单字候选里缩小范围 |
| 输入 | 中文标点 | 中文状态下的标点输出为全角 |
| 输入 | Shift 单击切中英 | 关闭后 `Shift` 只作修饰键 |
| 输入 | 输出繁体 | 候选与上屏改用繁体;字形地区可选通用、台湾、香港 |
| 候选 | 每页候选数 | 3、5、9 |
| 候选 | 下一词联想 | 按本机上屏词序补全下一词 |
| 候选 | 自动模糊音 | 默认开启;「重置」清空模糊组的使用统计 |
| 词库 | 自定义短语 | 打开 `phrases.txt`,保存后自动重载 |
| 词库 | 用户词库 | 导入、导出或清空本机学习记录 |
| 关于 | 引擎状态 | 词典来源与规模,语法模型、4M/25M 重排是否加载 |
| 关于 | 检查更新 | 手动检查 GitHub Releases |
| 关于 | 诊断信息 | 版本、配置、模糊组统计与服务日志末尾导出成一份文本 |

键盘操作:`Tab` / `Shift + Tab` 移动焦点,`空格` 或 `Enter` 激活,`←` / `→` 切换分段选项,`Esc` 关闭。

窗口高度按屏幕可用高度收缩,小屏或高缩放下卡片内部滚动,页脚按钮始终可见。

## 配置文件

`config.ini` 是行式 `键=值`,不解析行尾注释,未知键与坏值会被跳过。常用键:

```ini
page_size=5
scheme=full
aux_code=false
predict=true
fuzzy=auto
chinese_punct=true
shift_toggle=true
traditional=false
traditional_region=general
```

`scheme` 取 `full`、`xiaohe`、`ziranma`、`mspy`、`sogou`;`fuzzy` 取 `auto` 或 `off`(关掉自动模糊音);`traditional_region` 取 `general`、`tw`、`hk`。

直接编辑文件也可以;改动会在设置窗保存时或服务下次读取时生效。
