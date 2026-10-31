# 外部启动器集成（PowerToys 桥接）

掠影扩展命令不仅可在输入候选窗口与快捷键中触发，还可以通过命令行接口与 PowerToys Run、PowerToys 命令面板（Command Palette）或其他桌面启动器无缝集成。

## 核心桥接指令

输入法常驻服务对外暴露统一的 CLI 调度入口：

```sh
lueying ext run <完整命令ID>
```

例如调用官方截图 OCR 扩展：

```sh
lueying ext run shot-ocr.run
```

- 该指令通过命名管道向常驻服务提交执行请求，与输入法动作候选、全局快捷键复用相同的底层派发通道（`Runtime::invoke`）。
- **异步响应**：耗时较长的操作（如屏幕截图与 OCR 识别）由扩展沙箱与异步工作流在后台完成，结果通过系统通知或文字注入呈现，CLI 命令在请求投递后立即退出。

## PowerToys 命令面板接入方式

通过 PowerToys 命令面板（`Win + Alt + Space`）的「书签（Bookmark）」功能即可零依赖接入：

1. **查看命令标识**：运行 `lueying ext commands` 查看当前已加载的扩展命令列表及其完整 ID。
2. **创建 Shell 书签**：
   - 打开 PowerToys 设置 → 命令面板 → 书签。
   - 新建书签，名称填写「截图识别文字」，命令行填写：
     ```sh
     lueying.exe ext run shot-ocr.run
     ```
3. **通用动态调用书签**：
   - 若需创建可交互的通用调度入口，可将命令行设为：
     ```sh
     lueying.exe ext run {query}
     ```
   - 运行时面板将提示输入待调用的命令 ID。

## 安全与隔离保障

- **命令合法性校验**：仅允许触发已加载扩展清单中显式声明过的命令；未加载或不存在的标识直接报错。
- **隐私保护联动**：当前前台焦点处于密码框或系统敏感窗口时，后台请求将自动拒绝并记录审计。
- **审计追踪**：每次从外部触发的命令均按标准写入 `%LOCALAPPDATA%\lueying\logs\ext-audit.log` 审计日志。
