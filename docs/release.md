# 发布

发布由 tag 触发 `ci.yml` 的 Windows job。版本号、安装包、更新清单与 GitHub Release 都从同一个 tag 生成，避免多处手改。

## 发布前

1. 在 `CHANGELOG.md` 写 `## [x.y.z] - YYYY-MM-DD`，内容面向用户；发布工作流按 tag 提取这一段作为 Release 说明。
2. 确认 `main` 的 CI 通过，尤其是 Windows 全链路、32 位 TSF、安装/卸载 E2E。
3. 确认 `tools/fetch_*.sh` 钉住的词库、语法模型、重排模型与 OCR 模型仍可下载，SHA-256 未变。
4. 确认 `installer/lueying.iss` 的 `ArchitecturesAllowed` 与当前支持范围一致。ARM64 仍被拒绝安装。

Cargo 包版本不参与发布判定；`xtask pack --version` 用 tag 的版本，安装目录的 `version.txt` 是运行时权威。需要改 Cargo 版本时单独提交，别把它当发布开关。

## 打 tag

```sh
git tag v0.1.0
git push origin v0.1.0
```

tag 必须是 `vX.Y.Z` 或 `vX.Y.Z-预发布`。预发布段用 `-` 连接，例如 `v0.2.0-beta.1`；CI 会把预发布 Release 标记为 prerelease，稳定渠道不会把它当成 latest。

发布前 CI 会检查 `CHANGELOG.md` 是否有对应版本段；缺说明时在打包前失败。

## CI 产物

Windows job 依次完成：

- 构建 x64 Server、Settings、更新器、网络代理与用户态安装辅助程序。
- 用静态 CRT 构建 x64 与 i686 TSF DLL。
- 跑真实 TSF E2E、32 位 E2E、Inno 安装/升级/卸载 E2E。
- 检查运行时体积、安装包体积、内存与按键延迟门限。
- 下载并物化词典与模型，装配发布目录，生成 `version.txt` 与 `manifest.json`。
- 编译 `lueying-<version>-setup.exe`。
- 配置 `WINDOWS_PFX_BASE64` 与 `WINDOWS_PFX_PASSWORD` 时用 `signtool` 签安装包；未配置时发布未签名包。
- 生成 `lueying-manifest.json` 与 `.sha256`；配置 `LUEYING_SIGN_KEY` 时给清单加 Ed25519 签名。
- 创建或更新 GitHub Release，资产包括安装包、`.sha256`、更新清单与清单签名。

三个 secret 都是可选的，但正式发布建议全部配置：

| Secret | 作用 | 缺失时 |
|---|---|---|
| `WINDOWS_PFX_BASE64` | Authenticode 证书 PFX 的 base64 | 安装包未签名，SmartScreen 可能拦截 |
| `WINDOWS_PFX_PASSWORD` | PFX 密码 | 同上 |
| `LUEYING_SIGN_KEY` | 更新清单 Ed25519 私钥种子 | 清单不签名，客户端退回 `.sha256` |

## 发布后验证

在一台干净的 Windows 10/11 x64 上：

1. 安装 `lueying-<version>-setup.exe`，确认输入法列表出现「掠影」，64 位与 32 位宿主都能输入。
2. 检查安装目录 `version.txt` 与 Release 版本一致。
3. 从上一个稳定版本走「设置 → 关于 → 检查更新」，确认下载、SHA-256/清单验签、静默升级与版本切换。
4. 检查 `%APPDATA%\lueying` 的配置与学习记录在升级后保留。
5. 卸载，确认 TSF 注册、开机自启、开始菜单与用户数据按预期清理；被宿主占用的旧 DLL 允许重启后删除。

`workflow_dispatch` 只构建发布目录与安装包并上传 artifact，不创建 Release；用它在打 tag 前检查打包链路。

## 回退

客户端没有自动回退通道。已安装的新版本出问题时，发布一个更高版本修复，或在 Release 里撤下问题资产并写清原因。不要删除已被客户端引用过的更新清单签名文件；需要撤销时发布新清单并提升版本。

从旧的 zip 更新器切换到 Inno 安装包时，老客户端无法自动升级，需要手动安装一次。这个边界保留在 [architecture.md](architecture.md) 与 [install.md](install.md)。