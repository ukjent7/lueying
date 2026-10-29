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
| 候选 | 候选排列 | 横排挤一行自动换行,或竖排一行一条 |
| 候选 | 候选字号 | 小、标准、大;只缩放候选行,拼音行不变 |
| 候选 | 主题色 | 六种强调色预设;高对比度主题跟随系统色 |
| 候选 | 用户效果 | 打开 `%APPDATA%\lueying\effects` 目录,写 SkSL 装饰候选窗;见[用户效果](effects.md) |
| 候选 | 运行期脚本 | 打开 `%APPDATA%\lueying\extensions` 目录;用 `config.ini` 的 `script` 键选择启用的包,见[运行期脚本扩展](extensions/script.md) |
| 候选 | 下一词联想 | 按本机上屏词序补全 |
| 候选 | 自动模糊音 | 默认开启;「重置」清空模糊组的使用统计 |
| 词库 | 自定义短语 | 打开 `phrases.txt`,保存后自动重载 |
| 词库 | 自定义标点 | 打开 `punct.txt`,把半角符号映射成任意输出 |
| 词库 | 按应用配置 | 打开 `apps.ini`,单个应用关中文标点或不学习 |
| 词库 | 配置备份 | 配置、短语、标点、按应用与词库打包导出,导入时恢复 |
| 词库 | 用户词库 | 导入、导出或清空本机学习记录 |
| 关于 | 引擎状态 | 词典来源与规模,语法模型、4M/25M 重排是否加载 |
| 关于 | 更新渠道 | 稳定只认正式版;测试跟进预发布 Release |
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
ui_layout=horizontal
ui_font=standard
ui_accent=0
effects=on
update_channel=stable
```

`scheme` 取 `full`、`xiaohe`、`ziranma`、`mspy`、`sogou`;`fuzzy` 取 `auto` 或 `off`(关掉自动模糊音);`traditional_region` 取 `general`、`tw`、`hk`;`ui_layout` 取 `horizontal`、`vertical`;`ui_font` 取 `small`、`standard`、`large`;`ui_accent` 是主题色预设下标(0 起),也可以写颜色名 `蓝`、`青`、`绿`、`紫`、`红`、`橙`;`effects` 取 `on` 或 `off`(关掉用户效果);`update_channel` 取 `stable`、`beta`。

直接编辑文件也可以;改动会在设置窗保存时或服务下次读取时生效。

## 自定义标点与按应用配置

`punct.txt` 一行一条 `键=输出`:键是单个半角符号,输出是任意文本(至多 16 个字符)。表里的符号固定按映射上屏;表外走默认中文标点。示例:

```ini
,=、
^=………
```

`apps.ini` 节名是宿主进程的 exe 名(小写),可用键 `punct` 与 `learning`:

```ini
[notepad.exe]
punct=off
learning=off
```

没写的键跟随全局设置;覆盖在应用会话上线时生效,改完文件后点一次设置「保存」即可。

## 配置备份

「导出」把 `config.ini`、`phrases.txt`、`punct.txt`、`apps.ini` 与用户词库打包成一份文本;「导入」逐块写回文件、词库按合并方式恢复,最后自动重载。备份文件是普通文本,可以直接读;换机或重装时先装掠影,再导入一次即可。
