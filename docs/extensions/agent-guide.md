# Coding Agent 扩展开发指南

本文档面向使用 AI Coding Agent 或在 Linux 无头（Headless）终端环境下开发掠影扩展的开发者。通过掠影提供的 CLI 工具链，开发者无需依赖 Windows GUI 即可在本地完成「创建 → 编码 → 打包 → 安装 → 回放验证」的全流程。

## 核心开发工作流

`lueying ext` 命令行在 Windows 与 Linux 环境下具有完全一致的行为与输出：

```sh
# 1. 创建标准脚手架（生成 demo/manifest.json 与 demo/index.js）
lueying ext new demo

# 2. 编码与编译打包（生成纯文本格式的 demo.luext）
lueying ext pack demo

# 3. 安装到用户扩展目录（覆盖已有包需加 --force）
lueying ext install demo.luext --force

# 4. 通知常驻服务立即重读配置
lueying ext reload

# 5. 无头回放验证拼音行为与命令动作
lueying ext replay test_script.txt

# 6. 查看服务日志与运行指标
lueying ext logs --tail 50
lueying ext top
```

## 扩展包格式与规范

- **目录结构与命名**：扩展根目录名必须严格等于 `manifest.json` 中的 `id`（48 字节以内，仅允许小写英文字母、数字、`-` 与 `_`）。
- **纯文本归档（`.luext`）**：`lueying ext pack` 将扩展打包为带行数校验标记的纯文本格式（`### lueying-pack v1`）。打包过程仅允许文本资产（`.js`、`.json`、`.sksl`、`.txt`、`.md`），发现二进制文件将主动报错中断，方便代码审查与 Git 差异比对。

## 无头回放验证（Replay）

`lueying ext replay` 启动无头内置引擎，模拟真实击键并输出每一步的组合区状态与候选词列表，是编写端到端自动化测试与回归用例的核心工具：

```sh
# 从脚本文件运行回放
lueying ext replay test_script.txt

# 通过管道传入击键回放
echo "nihao" | lueying ext replay

# 注入特定触发词验证扩展动作候选
lueying ext replay --trigger ocr=shot-ocr.run test_script.txt
```

### 回放脚本编写语法

回放脚本每行代表一个输入阶段：
- `#` 开头为注释行，空行自动忽略。
- 普通字母与数字表示逐键敲击。
- `<键名>` 表示功能键：`space`、`enter`、`esc`、`back`、`delete`、`left`、`right`、`up`、`down`、`pageup`、`pagedown`、`shift`。
- 标点键：支持直接输入 `,`、`.`、`;`、`'`（隔音符）、`` ` ``（辅助码分隔符）等。

### 确定性回放输出示例

```text
> nihao
  组合 nihao · 候选 你好 / 你家 / nihao
> <space>
  上屏 你好
  组合 (无)
```

> [!TIP]
> 回放引擎采用隔离的纯净环境（内置基准微型词库、全拼方案、联想关闭），在任何平台均能产生完全确定性的输出结果，可直接作为回归测试断言。

## Agent 验收自查清单

在提交扩展代码前，建议按以下清单依次核验：

- [ ] **标识对齐**：扩展目录名与 `manifest.json` 的 `id` 完全一致。
- [ ] **最小特权**：`needs` 与 `capabilities` 仅申请真实使用的服务与权限，无多余声明。
- [ ] **沙箱合规**：脚本中无 `eval`、无动态 `import`、无绕过沙箱的原生调用企图。
- [ ] **边界覆盖**：通过 `lueying ext replay` 验证了常规打字、前缀匹配、触发词动作及空组合等场景。
- [ ] **指标健康**：运行 `lueying ext top` 确认无内存泄漏与单步超时记录。
