# 安装

## 系统要求

- Windows 11 24H2（build 26100）及以上，x64。
- 不支持 ARM64。
- 安装需要管理员权限。

## 安装

1. 下载 `lueying-<版本>-setup.exe` 并运行。默认安装到 `%ProgramFiles%\lueying`，同时注册 TSF 文本服务。
2. 按 `Win + Space`，在输入法列表中选择 **掠影**。

## 升级

直接运行新版安装包。TSF 动态库文件名带版本号（`lueying_tsf-<版本>.dll`），宿主进程仍占用旧文件时新旧版本并存；重启宿主应用或注销后完全切换。配置与词库保持不变。

## 卸载

在「设置 → 应用」中卸载「掠影」。卸载时运行 `lueying-user.exe --uninstall`，注销输入法并清理自启。被占用的旧文件可能要到下次重启才删除。

## 数据位置

| 路径 | 内容 |
|---|---|
| `%APPDATA%\lueying\config.toml` | 主配置 |
| `%APPDATA%\lueying\phrases.txt`、`punct.txt`、`apps.toml` | 自定义短语、标点、按应用配置 |
| `%APPDATA%\lueying\extensions\` | 运行期扩展与同意记录 `consent.json` |
| `%APPDATA%\lueying\secrets\` | 扩展的密钥类设置，仅当前用户可读 |
| `%APPDATA%\lueying\webdav-password.txt`、`webdav-state.txt` | WebDAV 口令与上次同步记录，不进配置文件 |
| `%LOCALAPPDATA%\lueying\user.tsv` | 用户词库：选词学习、造词、置顶 |
| `%LOCALAPPDATA%\lueying\logs\server.log` | 服务日志 |

词典与模型位于安装目录，随安装包分发。

## 网络与隐私

- 输入法本体不联网，不上传按键、候选或上屏文本。常驻服务端 `lueying-server.exe` 不链接 `winhttp`，CI 检查其导入表。
- 只有以下操作会联网：点击「检查更新」、下载官方扩展、WebDAV 同步，以及经 `lueying-broker.exe` 访问清单白名单内域名的扩展。
- 私密字段（如密码框）中不学习，候选文字也不交给扩展。
