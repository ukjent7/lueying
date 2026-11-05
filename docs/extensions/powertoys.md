# 启动器集成

外部启动器（如 PowerToys Command Palette）可以通过命令行调用掠影扩展的命令。

1. 列出可用命令：

   ```sh
   lueying ext commands
   ```

   输出每个已启用扩展的命令 id，形如 `shot-ocr.run`。

2. 执行命令：

   ```sh
   lueying ext run shot-ocr.run
   ```

启动器只负责调用。扩展的权限与沙箱规则不变，见[分层与沙箱](layers.md)。
