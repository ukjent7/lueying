# 第三方数据与参考

## 词典数据(GPL-3.0,随包分发并保留署名)

主词典由 [白霜拼音 gaboolic/rime-frost](https://github.com/gaboolic/rime-frost)(GPL-3.0)的
`cn_dicts`(base / 8105 / 41448 / ext / tencent)经 `xtask dict` 转换生成,脚本
`tools/fetch_frost.sh` 钉住 commit `fbdf9efc4a5036db5f37b97323e1b936e8fe993c`。
白霜词库基于高质量语料重新统计词频。升级词库只需改 `FROST_REV` 重跑管线。
`dicts/mini.txt` 为本项目自写的迷你词库,无外部版权。

本项目以 GPL-3.0-or-later 发布,与数据许可兼容。

## 架构参考(只读学习,未复制代码)

- [qingjian](https://github.com/qingjian-team/qingjian):TSF 薄壳 + Server 进程的切分、
  异步编辑会话(沉浸式应用同步会话会崩宿主)、TIP 能力类别注册、密码框放行等模式。
- [WindInput](https://github.com/huanfeng/WindInput)、[msime-windows](https://github.com/metasequoiaime/msime-windows):
  同样采用 TSF 薄壳 + 核心服务进程架构,佐证该拆分。
