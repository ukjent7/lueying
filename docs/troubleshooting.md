# 排障

## 输入法列表里没有「掠影」

- 确认系统是 Windows 11 24H2（build 26100）及以上的 x64 版本。
- 重新运行安装包进行修复。
- 升级后宿主进程仍占用旧 DLL 时，重启宿主应用或注销即可。

## 没有候选，或打字没有反应

常驻服务 `lueying-server.exe` 可能挂起。在任务管理器中结束它；下次在输入框中按键时，TSF 会重新启动服务。

- 服务启动完全没动静（任务管理器里一闪而过）：候选窗需要 UIAccess 资格，`lueying-server.exe` 带有 uiAccess 清单，**没有可信签名时系统会直接拒绝创建进程**。安装包会在安装时自动建证书并签名；本地构建直接运行时需要在管理员 PowerShell 里跑一次（证书已存在则复用）：

  ```powershell
  $c = Get-ChildItem Cert:\LocalMachine\My | Where-Object { $_.Subject -eq 'CN=lueying dev' } | Select-Object -First 1
  Set-AuthenticodeSignature -FilePath .\target\x86_64-pc-windows-gnu\release\lueying-server.exe -Certificate $c
  ```

## 候选质量差，只剩单字

安装目录缺少词典或模型时会降级。检查 `%ProgramFiles%\lueying` 中是否有：

- `main.pyd`、`collocation.bin`
- `rerank-8m.*`、`rerank-25m.*`（每组三个文件）

Windows 可在同一模型目录放置 `rerank-8m.packed.safetensors` 或
`rerank-25m.packed.safetensors`。服务优先只读映射预打包文件，缺失或无法解析时读取同目录的 ORT 文件。现有安装和下载流程仍提供 ORT；预打包文件是可选项，不要把未经转换的上游权重改名放进去。

```sh
cargo run --release -p lueying-rerank --example pack-model -- data/rerank rerank-25m data/rerank/rerank-25m.packed.safetensors
```

缺失时重新安装即可修复。

## 日志

- **服务日志**：`%LOCALAPPDATA%\lueying\logs\server.log`。超过 1 MiB 时滚动为 `server.log.1`，只保留一份归档。
- **按键追踪**：设置用户环境变量 `LUEYING_TSF_LOG=<文件路径>` 后重启目标应用。日志只含按键消息与编辑状态码，不含输入正文。排查结束后删除该变量。
- **上下文提取追踪**：设置环境变量 `LUEYING_DEBUG_UIA=1`，输出到系统调试流。

## 反馈问题

在「设置 → 关于」点击「导出诊断信息」，把生成的文本附在 Issue 中。
