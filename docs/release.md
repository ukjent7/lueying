# 版本发布

掠影采用 Git Tag 驱动的自动化持续交付流程。安装包构建、更新清单签名与 GitHub Release 发布均由 CI 工作流自动完成，确保交付物的一致性与可追溯性。

## 发布前检查清单

在推送发布 Tag 前，必须完成以下验证：

1. **更新日志**：在 `CHANGELOG.md` 中编写面向用户的 `## [x.y.z] - YYYY-MM-DD` 变更说明；CI 将提取该段内容作为 GitHub Release 的发版说明。
2. **主干质量状态**：确认 `main` 分支的持续集成（CI）全部通过，特别是真实 TSF E2E、32 位兼容性测试与安装/卸载冒烟测试。
3. **外部资产完备性**：确认 `tools/fetch_*.sh` 锁定的词典、语法搭配与重排模型地址依然有效，SHA-256 校验和保持一致。
4. **支持架构确认**：确认 `installer/lueying.iss` 的 `ArchitecturesAllowed` 符合支持预期（当前仅支持 x64）。

## Tag 规范与触发

```sh
git tag v0.1.0
git push origin v0.1.0
```

- Tag 名称必须遵循语义化版本格式：`vX.Y.Z`（正式版）或 `vX.Y.Z-<预发布标识>`（如 `v0.2.0-beta.1`）。
- 带有预发布标识的 Tag 会被 CI 自动标记为 Pre-release，不会被稳定版客户端作为 `latest` 拉取。
- 若 `CHANGELOG.md` 中缺少对应版本的变更描述，CI 打包任务将直接中止失败。

## 自动化流水线与产物

Windows CI 构建任务依次执行以下步骤：

1. **核心组件构建**：编译 64 位服务端、设置中心、独立更新器、网络代理及安装辅助程序。
2. **TSF 薄壳编译**：使用静态 C 运行时（Static CRT）构建 x64 与 i686（32 位）两份 `lueying_tsf.dll`。
3. **端到端测试与指标断言**：运行全链路宿主 E2E 测试，验证运行时体积（≤ 80 MiB）、常驻内存工作集（≤ 128 MB）与交互延迟（p95 ≤ 60ms）。
4. **安装包打包**：整合词典与模型资源，生成 `version.txt` 与结构化清单，编译输出 `lueying-<version>-setup.exe`。
5. **数字签名**：
   - **Authenticode 签名**：若配置了 `WINDOWS_PFX_BASE64` 与密码密钥，使用 `signtool` 对安装包进行数字签名。
   - **清单签名**：生成 `lueying-manifest.json` 与 `.sha256`，若配置了 `LUEYING_SIGN_KEY` 则追加 Ed25519 签名。
6. **创建 GitHub Release**：将安装包、校验文件、更新清单与签名上传至发布版本。

### 签名密钥说明

| 密钥名称 | 作用 | 缺失时的行为 |
|---|---|---|
| `WINDOWS_PFX_BASE64` | Authenticode 证书 PFX 编码 | 输出未签名安装包，Windows SmartScreen 可能出现拦截提示 |
| `WINDOWS_PFX_PASSWORD` | PFX 证书密码 | 同上 |
| `LUEYING_SIGN_KEY` | 更新清单 Ed25519 私钥种子 | 清单未签名，客户端降级使用基础 `.sha256` 校验 |

## 发布后验证流程

在干净的 Windows 10/11 x64 测试机上验证新版本：

1. 运行 `lueying-<version>-setup.exe`，确认安装顺畅，64 位与 32 位宿主应用均可正常唤出输入法。
2. 确认安装目录中 `version.txt` 与 Release 版本完全吻合。
3. 从上一稳定版打开「掠影设置 → 关于 → 检查更新」，验证清单校验、静默更新与版本平滑迁移。
4. 验证升级后 `%APPDATA%\lueying` 中的用户设置与自学习词库完整保留。
5. 执行卸载，验证 TSF 注册表项、服务自启与临时文件均按预期清理。

## 回滚与应急

客户端无自动降级逻辑。已发布的版本若发现致命缺陷：
- 建议立即发布更高的小版本号执行热修复。
- 或在 GitHub Release 中撤回受影响资产并发布置顶公告。严禁擅自删除已被客户端引用的 Ed25519 签名文件，以免造成旧版客户端更新链死锁。
