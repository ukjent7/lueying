# Linux(Wayland)外壳

`lueying-linux` 是掠影在 Linux 上的输入法进程:直接实现 Wayland 的
`zwp_input_method_v2`,在进程内跑完整引擎,候选窗用 `tiny-skia` 软件
绘制到 `wl_shm`,不依赖 GTK、Qt 或 ibus/fcitx。

## 运行要求

- 合成器实现 `zwp_input_method_v2` 与 `zwp_virtual_keyboard_manager_v1`。
  目前 Sway、Hyprland 等 wlroots 合成器可用。
- KDE Plasma 6 的输入法协议仍是 `zwp_input_method_v1`,本外壳暂不兼容;
  GNOME/Mutter 没有该协议,无法使用。
- 系统需安装 `libxkbcommon`(常见发行版默认已有)。

## 构建与安装

```sh
cargo build --release -p lueying-linux --locked
install -Dm755 target/release/lueying-linux /usr/local/bin/lueying-linux
```

词典与模型是可选的运行期数据,按以下顺序查找:

1. `LUEYING_DATA_DIR` 指向的目录
2. 可执行文件旁的 `../share/lueying` 与 `data`
3. `/usr/local/share/lueying`、`/usr/share/lueying`
4. 当前目录的 `data`

需要 `main.pyd`、可选的 `collocation.bin`,以及 8M/25M 两档重排模型
各自的三件套 `rerank-8m.ort` / `.model.json` / `.vocab.json` 与 `rerank-25m.*`。
缺词典时退回内置词典;缺模型时只用引擎排序。

在合成器配置里启动:

```sh
exec lueying-linux
```

## 配置与数据路径

遵循 XDG:

| 内容 | 路径 |
| --- | --- |
| 配置、短语、标点 | `$XDG_CONFIG_HOME/lueying/`(默认 `~/.config/lueying/`) |
| 用户词库、模型 | `$XDG_DATA_HOME/lueying/`(默认 `~/.local/share/lueying/`) |

环境变量:

- `LUEYING_CONFIG`:整体改写 `config.toml` 的位置(同目录的 `apps.toml`、`phrases.txt`、`punct.txt` 一并跟随)。
- `LUEYING_DATA_HOME`:改写用户数据根目录。
- `LUEYING_DATA_DIR`:指定词典与模型所在目录。
- `LUEYING_MODEL_DIR`:单独指定重排模型目录。
- `LUEYING_THEME=dark` 或 `GTK_THEME=*Dark*`:深色候选窗。

配置 schema 与 Windows、macOS 相同,见 `platforms/windows/server/src/config.rs`;
Linux 当前没有设置窗口,直接编辑 `config.toml` 后重启进程生效。

## 已实现

- 全拼、简拼、整句词图、模糊音、用户词库自学习。
- 双拼、辅助码、自定义短语、下一词联想、表情、英文混输、简繁转换。
- 8M 按键重排与 25M 停顿重排;模型缺失时自动跳过。
- 按键 grab、虚拟键盘回送、Shift 单击切换中英文、候选窗鼠标点选与翻页。
- 横排候选窗、浅色/深色配色、页标签。

## 已知边界

- 只支持 `zwp_input_method_v2`;`zwp_input_method_v1` 与 X11 不在本外壳范围。
- 没有图形设置界面,配置靠文件。
- 打包脚本与发行版仓库尚未提供;当前从源码构建。
