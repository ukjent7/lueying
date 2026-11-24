# 掠影外观验收记录

本记录以 macOS 26–27 的原生完成度为设计参照。Apple 没有为这个 Windows 输入法提供产品验收或认证；这里的「通过」只指下表中的掠影验收项和对应样本，不表示 Apple 认可，也不表示整个项目符合 WCAG。

## 标准来源

| 代号 | 一手来源 | 采用的原则 |
| --- | --- | --- |
| A1 | [Apple HIG: Layout](https://developer.apple.com/design/human-interface-guidelines/layout)、[Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars) | 用对齐、间距和分组传达信息层级；状态和命令能快速区分。 |
| A2 | [Apple HIG: Materials](https://developer.apple.com/design/human-interface-guidelines/materials)、[Color](https://developer.apple.com/design/human-interface-guidelines/color) | 克制使用玻璃与强调色；材质用于区分功能层，同时维持前景可读性。 |
| A3 | [Apple HIG: Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility) | 检查浅色和深色的对比；不要仅靠颜色表达信息；尊重系统辅助功能。 |
| A4 | [Apple HIG: Motion](https://developer.apple.com/design/human-interface-guidelines/motion) | 动效需要有目的，可以减少，且不能成为唯一反馈。 |
| W1 | [WCAG 2.2 1.4.3: Contrast (Minimum)](https://www.w3.org/TR/WCAG22/#contrast-minimum) | 本项目把普通文字至少 4.5:1 作为量化参考。 |
| W2 | [WCAG 2.2 1.4.11: Non-text Contrast](https://www.w3.org/TR/WCAG22/#non-text-contrast) | 识别控件、状态所需的非文字视觉线索至少 3:1；不套用于装饰线。 |

上述网页于 2026-11-24 通过 Pi 的 Firecrawl 搜索和抓取核对；Apple HIG 是持续更新的指南，不是 macOS 版本认证清单。

## 证据记录：2026-11-24

| 验收项 | 判据 | 结论及证据 |
| --- | --- | --- |
| L1 · 选择面板（A1） | 标题、列表、页脚各占独立区域；长文本截断，不覆盖计数；末行不裁切。 | **样本通过**：[浅色](evidence/picker-light.png)、[深色](evidence/picker-dark.png)、[系统配色](evidence/picker-contrast.png)。五行、长文本、五位数计数均出自真实 Rune 控件树。 |
| L2 · 工具栏（A1） | 状态、命令可分辨；三个缩放档不裁切、不碰撞，月相保留。 | **样本通过**：黑瓷 [75%](evidence/ceramic-075.png)、[100%](evidence/ceramic-100.png)、[150%](evidence/ceramic-150.png) 并排检查。只覆盖截图静止状态，不等同于 Windows 显示缩放、悬停和命中实测。 |
| L3 · 候选窗（A1、A2） | 横条和展开棋盘都能直接识别当前选中项；控制轨不遮盖候选文字；末行不裁切。 | **静态样本通过**：同一组候选的黑瓷 [横条](evidence/candidate-strip.png) / [棋盘](evidence/candidate-grid.png)。展开时仍选中「略影」，24 个可见候选独立于右侧控制轨。仅覆盖首屏、默认字号和单一选中位置。 |
| L4 · 字号与竖排（A1、A3） | 大字号竖排的九行均完整可见；长词用省略号截断；选中胶囊完整。 | **静态样本通过**：[候选窗](evidence/candidate-column-stress.png)及 [760 逻辑像素宽设置页](evidence/settings-candidate-stress.png)。[行距修复前](evidence/candidate-column-stress-before-spacing.png)取自外壳坐标已修正、但末行仍被裁掉的中间构建；新增布局测试覆盖三档倍率的竖排与棋盘。 |
| L5 · 设置导航与分组（A1） | 页头明确当前位置；相关控件成组；最小窗口的首屏中标题、搜索框、可见控件与页脚不互相遮挡。 | **静态首屏样本通过**：[输入](evidence/settings-nav-input.png)、[候选](evidence/settings-nav-candidate.png)、[扩展商店](evidence/settings-nav-extensions.png)、[词库](evidence/settings-nav-dictionary.png)、[关于](evidence/settings-nav-about.png)、[跨页搜索](evidence/settings-nav-search.png)。[候选改前](evidence/settings-nav-candidate-before.png)与[改后宽窗](evidence/settings-nav-candidate-wide.png)供对照。模型测试另查输入与候选页的可见选项未重复或遗漏。不涵盖滚动全程、键盘焦点、空搜索结果或原生显示缩放。 |
| L6 · 设置键盘路径（A3） | 能用键盘切分类、找到首屏外的选项并操作；焦点滚入可见区域；搜索框回车不会触发保存。 | **所测 Rune 路径通过**：[Windows 调试自检](evidence/settings-keyboard-selftest.txt)在 760×480 的离屏控件树中，从搜索切到候选页；Tab 到「下一词联想」时开关完整进入视口，空格确实切换；页脚支持 Tab、Shift+Tab 和聚焦时回车。保存仅验证派发到了真实回调，未写配置，也未实际关闭窗口。焦点环的像素对比、原生键盘事件和读屏仍待验。 |
| C1 · 选中文字（A3、W1） | 实心主题色上的普通文字不少于 4.5:1。 | **色值通过**：6 个预设各自浅/深值，12 个样本最低 **4.5479:1**；默认蓝 `#2f6fed` 与白字的组合是最低值。它不依赖玻璃背景。 |
| C2 · 实底选中轮廓（A3、W2） | 用于识别选中的填充与相邻实底至少 3:1。 | **色值通过**：6 个预设最低值分别为浅色选择面板 `#ffffff` 上 **4.5479:1**、深色选择面板 `#202127` 上 **3.5295:1**、深色预设配黑瓷 `#1b1c20` 上 **3.7433:1**。动态玻璃和扩展传入的任意主题色不在这组数据内。 |
| C3 · 选择面板元数据（A3、W1） | 数量、位置与快捷提示达到 4.5:1。 | **默认色值通过**：浅色 `#6a6e77` / `#ffffff` 为 **5.1100:1**，深色 `#8e929c` / `#202127` 为 **5.1533:1**。 |
| C4 · 系统辅助功能（A3） | 切换系统高对比、关闭透明度后，文字、状态和控件仍可辨。 | **部分有证据**：[选择面板](evidence/picker-contrast.png)、候选窗 [横条](evidence/candidate-strip-contrast.png) / [棋盘](evidence/candidate-grid-contrast.png) 走系统色分支。[修复前棋盘](evidence/candidate-grid-contrast-before.png) 的箭头和滚动滑块走低强调灰色；修复后均使用实色系统正文色，滚动滑块未被轨道底色盖住。普通黑瓷两张截图逐字节不变。未切换 Windows 系统高对比主题，也未测试其他系统配色、关闭透明度或实机命中。 |
| M1 · 活玻璃（A2、A3、W1、W2） | 明暗和高频背景中不露角，文字、命令图标和选中态始终可读；黑瓷和暖月辨识度保留。 | **部分有证据**：工具栏 [合成暗底](evidence/glass-dark-100.png) / [合成亮底](evidence/glass-light-100.png) 静态样本中背板铺满轮廓；候选窗 [滚动条纹样本](evidence/candidate-glass-up-live.png) 中，收起时背板既铺满外壳，也继续更新。尚未验证原生桌面捕获、采集排除、系统合成、实时性能或各元素的动态对比。 |
| M2 · 动效（A4） | 展开/收起时，选中底色始终对应选中文字；旧控件不残留。减少动态效果时，面板不做空间形变。 | **部分有证据**：黑瓷向下展开的 [普通动效](evidence/candidate-motion-normal.png) / [减少动态效果](evidence/candidate-motion-reduced.png)，合成玻璃向上展开的 [普通动效](evidence/candidate-glass-up.png) / [减少动态效果](evidence/candidate-glass-up-reduced.png)。采样帧中内容保持对齐；玻璃向上收起时外壳底边留在固定窗口底边。真实光标定位、同页选中位移和工具栏月相尚未验证。 |
| X1 · 一致性（A1–A3） | 候选条、展开棋盘、工具栏、选择面板和设置并排观察时属于同一产品，但不能抹掉中文状态字、暖月、玻璃与黑瓷的区别。 | **部分有证据**：对照 [设置预览](evidence/settings-nav-candidate-wide.png)、[候选横条](evidence/candidate-strip.png)、[工具栏](evidence/ceramic-100.png)和[选择面板](evidence/picker-dark.png)。预览复用候选行矩形、字号比例和圆角；工具栏保留中文状态和月相，选择面板保留独立的标题、列表和页脚。真实桌面上的组合效果尚未验证。 |

色值来自 [`accents.rs`](../../platforms/windows/server/src/accents.rs) 和 [`theme.rs`](../../platforms/windows/server/src/theme.rs)。以 WCAG 的 sRGB 相对亮度计算 `(L亮 + 0.05) / (L暗 + 0.05)`，在比较阈值后才四舍五入。色值计算不是屏幕像素测量，不涵盖抗锯齿、悬停、合成或系统自定义配色。

## 复现静态快照

以下命令在 Windows 调试构建使用实际 UI 绘制函数。它不启动常驻服务、不替换输入法，内容为固定样例，不采集用户剪贴板或桌面。`contrast` 强制走系统配色分支，不切换系统设置。工具栏百分比是应用内缩放；PNG 以 2× 设备倍率生成，玻璃用合成条纹供给生产光学和裁切路径。

```powershell
$env:LUEYING_NO_UIACCESS = "1"
cargo build --locked --target x86_64-pc-windows-gnu -p lueying-server -p lueying-settings
$exe = ".\target\x86_64-pc-windows-gnu\debug\lueying-server.exe"
& $exe --visual-picker light picker-light.png
& $exe --visual-picker dark picker-dark.png
& $exe --visual-picker contrast picker-contrast.png
& $exe --visual-toolbar ceramic 0.75 ceramic-075.png
& $exe --visual-toolbar ceramic 1 ceramic-100.png
& $exe --visual-toolbar ceramic 1.5 ceramic-150.png
& $exe --visual-toolbar glass-dark 1 glass-dark-100.png
& $exe --visual-toolbar glass-light 1 glass-light-100.png
& $exe --visual-candidate strip candidate-strip.png
& $exe --visual-candidate grid candidate-grid.png
& $exe --visual-candidate column-stress candidate-column-stress.png
$settings = ".\target\x86_64-pc-windows-gnu\debug\lueying-settings.exe"
& $settings --visual-settings-candidate default settings-candidate.png
& $settings --visual-settings-candidate stress settings-candidate-stress.png
foreach ($scene in 'input','candidate','extensions','dictionary','about','search') {
    & $settings --visual-settings-page $scene "settings-nav-$scene.png"
}
& $settings --visual-settings-candidate default settings-nav-candidate-wide.png
& $exe --visual-candidate strip-contrast candidate-strip-contrast.png
& $exe --visual-candidate grid-contrast candidate-grid-contrast.png
```

候选窗快照走生产控件树，但固定在静止态，不接入生产事件循环；修复前的棋盘留作历史对照，当前构建不复现旧图。

## 复现设置键盘路径

沿用上节的 Windows 调试构建，在 PowerShell 中运行：

```powershell
$report = Join-Path $env:TEMP "lueying-settings-keyboard-selftest.txt"
Start-Process -FilePath $settings -ArgumentList "--selftest", ('"{0}"' -f $report) -Wait
Get-Content -Encoding UTF8 $report
```

报告应包含 `结果:通过`；`键盘:` 各行列出实际焦点路径及滚动量。自检不打开设置窗口，不写用户配置，也不会通知常驻服务重读。

## 复现候选窗动效对照

以 60 fps 虚拟时钟渲染，0.25 秒请求展开，1.00 秒请求收起。帧带从左到右、从上到下读取，每三帧保存一张（间隔 50 ms，34 张）。使用黑瓷、固定大小窗口、向下展开；走生产控件树和 Rune 动画，调试驱动模拟形态切换，未经过 TSF 或系统窗口合成。

- **普通动效**：[修复前](evidence/candidate-motion-normal-before.png)第 18 帧可见胶囊先换位置、文字还在滑，第 60 帧选中文字落在胶囊外；[修复后](evidence/candidate-motion-normal.png)文字与胶囊同帧对齐，只让外壳改变高度。展开途中，尚未展开到的行仍会被外壳裁切。
- **减少动态效果**：[修复前](evidence/candidate-motion-reduced-before.png)第 60 帧还能看到滑块残段；[修复后](evidence/candidate-motion-reduced.png)收起帧只剩横条控件。采样帧中没有中间高度。

```powershell
New-Item -ItemType Directory -Path motion-normal,motion-reduced
& $exe --visual-candidate-motion normal candidate-motion-normal.png motion-normal
& $exe --visual-candidate-motion reduced candidate-motion-reduced.png motion-reduced
```

### 向上展开与玻璃背板

用同一虚拟时间表，把生产光学处理的输入换为按桌面坐标生成的三色明暗条纹，面板贴固定窗口底边向上展开。对照收起的第 60 帧：[修复前](evidence/candidate-glass-collapse-before.png)外壳仍接近棋盘高度，背板却已换成横条大小；[首次修复后](evidence/candidate-glass-collapse.png)外壳内的条纹保持连续，但背板像素会短暂停在旧画面。

现在普通动效同窗收起时，保留的是原棋盘**采样范围**（450 ms），不是旧像素；仍按原有节奏更新，随后恢复为横条采样。合成条纹在虚拟时间滚动，[完整帧带](evidence/candidate-glass-up-live.png)和第 [60 帧](evidence/candidate-glass-live-0060.png) / [66 帧](evidence/candidate-glass-live-0066.png)显示收起期间纹理继续变化。两帧中 `(x=240, y=166)` 的 PNG RGB 从 `(88, 43, 28)` 变为 `(68, 36, 25)`；同坐标静止条纹对照仅从 `(56, 31, 21)` 变为 `(58, 33, 22)`（外壳压光也随尺寸变化）。两帧在 `x=240` 的最后一个非透明像素都在 `y=209`，即该样本的窗口底边；这不是 TSF 光标位置测量。原有静止条纹普通及减动态帧带均逐字节不变。这些证据验证离屏渲染，不替代 Windows 实机录屏。

```powershell
New-Item -ItemType Directory -Path glass-up,glass-up-reduced,glass-up-live
& $exe --visual-candidate-motion glass-up candidate-glass-up.png glass-up
& $exe --visual-candidate-motion glass-up-reduced candidate-glass-up-reduced.png glass-up-reduced
& $exe --visual-candidate-motion glass-up-live candidate-glass-up-live.png glass-up-live
```
