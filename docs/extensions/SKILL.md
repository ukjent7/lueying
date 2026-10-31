# 掠影运行期扩展开发规范（Skill 模板）

本模板为开发人员或 Coding Agent 提供创建掠影运行期扩展的标准开发规范与验收标准。最终交付物为符合规范的扩展目录（包含 `manifest.json` 与 `index.js`），支持用户在设置中心授权后即开即用。

## 架构与安全前提

1. **单文件构建**：QuickJS 沙箱不支持运行时 `import`；请先使用 TypeScript 编写源码，再使用 esbuild 等构建工具打包为单文件 IIFE 格式。
2. **零敏感数据暴露**：外观处理器与命令无法读取用户的输入拼音、候选汉字列表或宿主应用名称。
3. **隐私模式静默**：密码框与系统隐私字段下宿主自动熔断扩展逻辑，不执行任何扩展命令。
4. **受控服务原语**：所有涉及截屏、剪贴板、离线 OCR 或网络的外联能力必须通过 `lueying.<service>.*` 原语调用。

## 目录组织标准

```
my-extension/
  ├── manifest.json       # 扩展元数据、激活条件、权限与声明式设置
  ├── index.js            # 打包后的单文件 IIFE 运行代码
  └── README.md           # 面向用户的扩展说明（用途、权限、数据去向）
```

> [!IMPORTANT]
> 扩展目录名称必须与 `manifest.json` 中的 `id` 字段严格保持一致。

## 清单模板 `manifest.json`

```json
{
  "id": "my-extension",
  "name": "我的扩展",
  "version": "1.0.0",
  "api": 2,
  "main": "index.js",
  "activate": {
    "triggers": ["my"],
    "hotkeys": ["Ctrl+Alt+M"],
    "commands": [
      { "id": "run", "title": "执行快捷任务", "when": "any" }
    ]
  },
  "needs": ["clipboard", "notify"],
  "capabilities": ["clipboard"],
  "settings": [
    {
      "key": "show_notification",
      "title": "任务完成后提示通知",
      "type": "toggle",
      "default": true
    }
  ]
}
```

## 核心实现示例 `index.js`

```javascript
// 注册命令实现
lueying.command("run", async () => {
  const { text } = await lueying.clipboard.read();
  await lueying.notify.show({
    title: "扩展执行成功",
    body: `处理了 ${text.length} 个字符`
  });
  return { processed: text.length };
});

// 可选：注册候选窗动态装饰外观
lueying.register((scene) => {
  if (scene.high_contrast) return null;
  return {
    accent: scene.dark ? 0x64b5f6 : 0x1976d2
  };
});
```

全局类型声明参见同目录下的 [`lueying.d.ts`](lueying.d.ts)。

## 构建、打包与验证流程

```sh
# 1. 使用 esbuild 将 TypeScript 源码打包为单文件 IIFE
npx esbuild src/index.ts --bundle --format=iife --minify --outfile=index.js

# 2. 本地打包验证并安装
lueying ext pack my-extension
lueying ext install my-extension.luext --force
lueying ext reload

# 3. 使用回放引擎验证打字交互与动作候选
lueying ext replay test_script.txt

# 4. 实时监控运行状态与日志
lueying ext logs --tail 20
lueying ext top
```

## 交付验收清单

- [ ] 目录名与 `manifest.json` 中的 `id` 严格吻合。
- [ ] `manifest.json` 格式符合规范，且仅申请业务必需的 `needs` 与 `capabilities`。
- [ ] 未使用 `eval`、动态导入、原生直连等绕过沙箱的操作。
- [ ] 所有命令均在 `activate.commands` 中声明了清晰的中文标题。
- [ ] 提供面向用户的 `README.md`，清楚告知权限用途与数据存储位置。
- [ ] 本地通过 `lueying ext replay` 验证通过，无未捕获异常。
