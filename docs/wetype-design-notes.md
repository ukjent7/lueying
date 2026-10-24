# 微信输入法(WeType)设计参考笔记

来源:官方安装包 `WeTypeSetup_official_p.exe` 3.0.0.38(NSIS 自解压,7z 直接解包)。
只提炼设计与工程事实,用于对照我们的方案;不使用其任何代码/数据/资源。
二进制未深加壳,导入表可读;无需 IDA 深挖,组件布局已足够回答设计问题。

## 组件与进程架构(成熟输入法的拆分)

```
wetype_tip_{x86,x64,arm,arm64,arm64ec}.dll  TSF 薄壳,每架构一份(~2.4MB)
CrashRpt1500.dll / CrashSender1500.exe      崩溃捕获与上报,直接挂在 TIP 上
wetype_server.exe                            引擎/会话服务进程(7.8MB)
wetype_renderer.exe                          独立渲染进程,专画候选窗等 UI
wetype_service.exe                           Windows 服务(提权/更新场景)
wetype_update.exe                            升级器
设置界面                                      Flutter(flutter_windows.dll + flutter_datas)
```

与我们方案的对照:

- **TIP 薄壳 + Server 进程**:与青简/WindInput/水杉一致,五家同构;印证我们
  M4 起的 server + GPUI 拆分。WeType 更进一步把「画窗」拆成独立 renderer,
  候选窗崩溃不连累引擎进程——GPUI 窗口先内嵌在 server,M6 再评估是否单独进程。
- **崩溃兜底分两层**:CrashRpt 挂在 TIP(宿主进程内的任何崩溃先就地捕获上报),
  引擎在 server(崩了不连累宿主)。我们单 DLL 阶段靠纯 Rust + catch_unwind 兜底,
  拆 server 后天然获得第二层。
- **arm64ec**:Win11 on ARM 的 x64 互操作进程会加载 arm64ec 版输入法。
  我们的发布矩阵应至少 x64 + arm64,arm64ec 列入 M6。

## 词典与数据部署

- 词典全部预编译为二进制分文件,`BuildIn/index.json` 是清单:每文件
  md5 / type / uniqueID / version / timestamp / support_update / is_cell,
  支持词典单独热更新(不随主程序)。
- 功能分文件:主词库、**简拼单独一份**(bwcjpmac_jianpin)、语言模型(lmcnmac)、
  神经模型(nlrgpt2 + tensorflowlite)、拼音编辑表(editpinyin/ln2py)、细胞词库。
  简拼独立成表 = 简拼查询量大到值得物化,印证我们的「签名索引」方向;
  规模大了以后可同样物化。
- 安装器带 `channel.ini`(发布渠道)与 crashrpt 多语言 ini。

## 有意不跟的部分

- 云同步/语音/翻译/表情/AI 联想/神经 LM:差异化能力,不属于「基础体验打磨」目标。
- Flutter 设置界面:我们用 GPUI 统一。
- 自动更新器与服务:发布到 GitHub Releases 后再议。
