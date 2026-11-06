# 架构

## 进程

```
lueying_tsf.dll        TSF 薄壳，载入宿主进程，只依赖 lueying-protocol
    │  命名管道 \\.\pipe\lueying-v2
    ▼
lueying-server.exe     常驻服务：引擎会话、候选窗、后台重排；不联网
lueying-settings.exe   设置窗（Slint）：配置、扩展、更新检查与安装
lueying-broker.exe     扩展联网的唯一出口，按清单白名单逐请求检查
lueying-user.exe       安装辅助：注册输入法、自启、卸载清理
```

- 宿主进程只转发按键并执行上屏，引擎与绘制都在服务端。
- 每个宿主线程对应一个引擎会话；词典、模型与用户词库在服务端进程内共享。
- 原生扩展（OCR、语音识别）是独立进程，按需启动，在 AppContainer 与 Job Object 限额内运行。

## 代码分层

| 层 | 目录 | 职责 |
|---|---|---|
| 词典 | `crates/dict` | `main.pyd` 的内存映射读取与校验 |
| 引擎 | `crates/engine` | 音节切分、词图解码（束宽 24）、纠错、用户词库学习 |
| 模型 | `crates/lm`、`crates/rerank` | 8M 与 25M 模型推理及重排决策；`lm` 的来源见其 README |
| 功能 | `crates/features` | 快捷输入、符号、表情、联想、简繁 |
| 会话 | `crates/session` | 候选视图同步与重排决策，Windows 与 macOS 共用 |
| 渲染 | `crates/render` | 候选窗与面板的布局、命中测试与绘制指令 |
| 协议 | `crates/protocol` | 管道帧、`SessionView`、按键 |
| 扩展 | `crates/ext*`、`crates/extwasm` | 清单、协议、宿主与服务，见[扩展](extensions/README.md) |
| 联网 | `crates/net`、`crates/broker` | 更新与下载、扩展联网代理 |
| 平台 | `platforms/windows/*`、`platforms/macos/ime`、`platforms/linux/ime` | 各平台外壳 |

核心 crate 不依赖任何平台目录；平台目录依赖核心 crate。

## 词典与模型

- **词典**：`main.pyd`（mmap 二进制）与搭配表 `collocation.bin`，由 [lueying-dict](https://github.com/ukjent7/lueying-dict) 生成。`tools/fetch_dict.sh` 固定版本与 SHA-256，当前为 v0.2.0。
- **重排模型**：8M（每键同步）与 25M（停顿后异步）各由 `.ort`、`.model.json`、`.vocab.json` 三个文件组成，由 `tools/fetch_rerank.sh` 获取，随安装包分发。
- **降级**：词典损坏或缺失时退回内置的小词典；模型缺失时只用词频与搭配排序，输入照常可用。

## 一次输入的路径

1. **按键**：TSF 发送 `Key`，引擎同步更新组合与候选，返回 `SessionView`。
2. **8M 同步重排**：在后台线程计算。结果只有在上下文序号与候选版本仍然匹配时才生效。
3. **25M 停顿重排**：停顿 120 ms 后计算。结果先记为待生效的首选，等 TSF 下一次 `Sync` 与内联文字一起切换，避免候选窗先变而上屏的仍是旧首选。
4. **上屏**：`Commit` 写回宿主，选词结果写入用户词库。快捷输入的候选不写入。

## 隐私与离线

- `lueying-server.exe` 不链接网络库，CI 检查其导入表。
- 私密字段中，候选文字留空，不交给扩展。
- 扩展只能使用清单声明且用户已同意的服务与权限。

## 质量门禁

| 指标 | 门限 | 位置 |
|---|---|---|
| 运行时体积 | ≤ 90 MiB | `.github/workflows/ci.yml` |
| 安装包 | ≤ 200 MB | `xtask/src/pack.rs` |
| 8M 决策 | p95 ≤ 40 ms，p99 ≤ 80 ms | `crates/e2e` |
| 25M 停顿重排 | p95 ≤ 800 ms | `crates/e2e` |
| 提交上屏 | p95 ≤ 60 ms | Windows E2E |
| 每键往返 | p95 ≤ 60 ms | Windows E2E |
| 服务端峰值工作集 | ≤ 160 MB | Windows E2E |
| 候选准确率 | 见[评测](benchmark.md) | `tools/bench_shared.py --gate` |
