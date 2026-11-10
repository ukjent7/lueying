# 发布

发布由 Git 标签触发。安装包、更新清单与 GitHub Release 由 CI 生成。

## 发版前

1. 在 `CHANGELOG.md` 写 `## [x.y.z] - YYYY-MM-DD` 段落。CI 用它作为 Release 说明，缺失时打包失败。
2. `main` 的 CI 与 `quality` 全部通过，包括 Windows E2E 与安装卸载冒烟（`quality` 只在核心路径改动时自动跑，发版前可手动触发）。
3. `tools/fetch_*.sh` 中固定的词典、模型与官方扩展哈希仍有效。

## 打标签

```sh
git tag v0.2.0
git push origin v0.2.0
```

`vX.Y.Z` 为正式版。`vX.Y.Z-<标识>`（如 `v0.3.0-beta.1`）标为预发布，稳定渠道不会拉取。

## 产物

| 文件 | 说明 |
|---|---|
| `lueying-<版本>-setup.exe` | 安装包（x64） |
| `*.sha256` | 安装包的校验值 |
| `lueying-manifest.json` | 更新清单；配置了 `LUEYING_SIGN_KEY` 时附 Ed25519 签名 |
| `lueying-ppocr.exe`、`lueying-sensevoice.exe` | 原生官方扩展；随各自的 Release 单独发布 |

`cargo run -p xtask -- winget` 生成 winget manifest，需要另外向 microsoft/winget-pkgs 提交 PR。

## 签名

| 密钥 | 作用 | 缺失时 |
|---|---|---|
| `WINDOWS_PFX_BASE64`、`WINDOWS_PFX_PASSWORD` | Authenticode 签名 | 安装包不签名，SmartScreen 可能拦截 |
| `LUEYING_SIGN_KEY` | 清单的 Ed25519 私钥种子 | 清单不签名，客户端只做 `.sha256` 校验 |

## 发布后验证

在干净的 Windows 11 24H2 x64 上：

1. 安装新版，输入法可用。
2. 安装目录中的 `version.txt` 与 Release 版本一致。
3. 从旧版「关于」检查更新，验证清单与升级。
4. 确认 `%APPDATA%\lueying` 中的配置与 `user.tsv` 保留。
5. 卸载，确认输入法注册与自启项已清理。

## 回滚

客户端没有自动降级。发现严重问题时发布更高的修订版本。确需撤回时，在 Release 中下架资产并置顶说明。不要删除已发布的清单签名，否则旧版本的更新链会断。
