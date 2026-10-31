# 运行期脚本扩展规范

掠影支持基于 QuickJS 沙箱的运行期 JavaScript 脚本扩展。扩展可在独立工作线程中动态渲染候选窗口外观、注册全局命令、响应输入触发词并调用系统原语服务。

## 扩展包目录结构

扩展统一存放于 `%APPDATA%\lueying\extensions\<id>\`：

```
%APPDATA%\lueying\extensions\my-pack\
  ├── manifest.json   # 扩展清单（元数据、权限、服务与命令声明）
  └── index.js        # 脚本入口（单文件 IIFE 格式）
```

- **目录命名**：必须严格等于清单中的 `id`（仅允许小写英文字母、数字、短横线 `-` 与下划线 `_`，最大 48 字节）。
- **打包规范**：`index.js` 必须为打包好的单文件自执行函数（IIFE），输入法内部不提供模块加载器或转译器。
- **注册契约**：入口脚本在顶层必须同步调用 `lueying.register(handler)` 注册外观处理器，或调用 `lueying.command(name, fn)` 注册命令。

## 启用与组合

在图形设置页「扩展」中勾选，或直接在 `%APPDATA%\lueying\config.toml` 中配置加载顺序：

```toml
extensions = ["shot-ocr", "my-pack"]
```

多个扩展按照数组顺序流水线执行，后序扩展返回的非空外观补丁字段将覆盖前序结果。扩展文件更新后，常驻服务通过内容哈希自动触发热重载。

## 清单定义 `manifest.json`

采用现代标准的 API 2 格式：

```json
{
  "id": "my-pack",
  "name": "示例扩展",
  "version": "1.0.0",
  "api": 2,
  "main": "index.js",
  "activate": {
    "triggers": ["my"],
    "hotkeys": ["Ctrl+Alt+M"],
    "commands": [
      { "id": "run", "title": "执行示例命令", "when": "any" }
    ]
  },
  "needs": ["notify", "clipboard"],
  "capabilities": ["clipboard"],
  "settings": [
    { "key": "notify", "title": "完成后发送通知", "type": "toggle", "default": true },
    {
      "key": "mode",
      "title": "运行模式",
      "type": "choice",
      "options": ["fast", "deep"],
      "default": "fast"
    }
  ]
}
```

### 声明式设置规范

扩展可在 `settings` 数组中声明配置项，由宿主自动在设置中心渲染，避免扩展自绘配置界面：

| 控件类型 | 对应数据类型 | 说明 | 存储位置 |
|---|---|---|---|
| `toggle` | `boolean` | 开关按钮，默认值为布尔值 | `config.toml` 中的 `[ext.<id>]` |
| `choice` | `string` | 单选分段控件，需提供 1~8 个选项 | `config.toml` 中的 `[ext.<id>]` |
| `secureText` | `string` | 敏感密钥文本（如 API Key），原生单行密文框 | `%APPDATA%\lueying\secrets\<id>.json`（安全隔离存储） |

## 命令生命周期与交互

```js
lueying.command("run", async (args) => {
  const clip = await lueying.clipboard.read();
  await lueying.notify.show({ title: "我的扩展", body: `已读取: ${clip.text}` });
  return { success: true };
});
```

- **按需激活与空闲释放**：仅包含命令的扩展在被触发时懒加载，连续空闲 30 秒后自动卸载 JS 运行时以回收内存。
- **异步支持与限额**：命令支持返回 Promise。等待外部异步服务（如 OCR 识别）不消耗脚本指令预算；但在 JS 沙箱内单步执行耗时仍严格受 8ms 限制。
- **三种触发通道**：
  1. **快捷键触发**：用户在 `config.toml` 的 `[bindings]` 中将热键绑定至完整命令 ID（`<扩展id>.<命令id>`）。
  2. **动作候选触发**：用户在 `[triggers]` 中配置触发词，键入完整触发词后该命令将作为动作条目显示在候选窗第 2 位。
  3. **命令面板检索**：按下总热键唤起命令面板，实时模糊检索并执行全部已声明命令。

## 宿主安全服务原语

清单 `needs` 声明的服务可在脚本中通过 `lueying.<service>.<method>(args)` 调用：

| 原语服务 | 核心方法 | 载荷与说明 |
|---|---|---|
| `capture` | `shot({region?})` | 截取屏幕区域，返回临时位图路径 `{file, width, height}` |
| `ocr` | `recognize({file, lang?, engine?})` | 离线文字识别；`engine` 可选 `"system"` 或 `"ppocr"` |
| `clipboard` | `read()` / `write({text})` | 纯文本读写剪贴板（读取属高敏感权限） |
| `store` | `get({key})` / `set({key, value})` / `remove({key})` / `list()` | 扩展专属本地 JSON 键值存储（单值 ≤ 64 KiB，总计 ≤ 256 KiB） |
| `output` | `deliver({text})` | 向当前焦点文本控件注入 Unicode 文本上屏 |
| `panel` | `show({title, items})` | 弹出原生简单列表选择面板并返回选中索引 |
| `net` | `fetch({url, method?, headers?, body?})` | 受限网络请求；必须预先在清单 `network.domains` 声明白名单 |
| `notify` | `show({title, body})` | 发送系统级通知气泡 |

> [!IMPORTANT]
> 常驻核心进程不引入任何网络代码。`net.fetch` 调用由独立代理进程 `lueying-netproxy.exe` 转发，仅放行清单域名白名单中的 HTTPS 目标。

## 候选窗外观动态装饰

```js
lueying.register((scene) => {
  if (scene.high_contrast) return null; // 高对比度模式下主动让位
  return {
    accent: scene.dark ? 0x8ab4f8 : 0x2f6fed,
    effects: {
      background: `
        uniform float2 u_size;
        uniform float u_anim;
        uniform float4 u_accent;
        half4 main(float2 p) {
          float a = 0.4 * u_anim;
          return half4(u_accent.rgb * a, a);
        }
      `
    }
  };
});
```

- **场景状态（`scene`）**：包含 `dark`（深色模式）、`high_contrast`（高对比度）、`time`（动画时钟）、`anim`（过渡进度）、`page`/`pages` 等环境状态。状态内**绝对不包含**用户敲击的拼音、候选汉字或宿主程序名。
- **装饰补丁（`Patch`）**：返回 `accent` 强调色与 SkSL 槽位代码。拼音字母与候选文字由输入法在上层独立绘制，确保视觉可读性。

## 监控指标与故障排查

常驻服务每秒输出监控快照至 `%LOCALAPPDATA%\lueying\logs\ext-metrics.json`：
- 执行 `lueying ext top` 可实时监控扩展内存占用、派发峰值、超时与故障次数。
- 服务端连续 3 次捕获扩展超时或崩溃后，自动隔离停用该扩展并将原因记录于 `last_error`。
