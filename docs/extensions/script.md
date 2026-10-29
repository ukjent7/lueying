# 运行期脚本扩展

候选窗的外观可以交给一段 JS 决定。扩展只改主题色与三个 SkSL 槽位,看不到候选文字,也拿不到宿主应用名。

> 状态:内核(`lueying-extjs`)、服务端加载、Windows 自检与设置页选择器已就绪;Linux E2E 覆盖坏清单、坏补丁、死循环、内存暴涨、隐私与热重载。

## 包结构

```
%APPDATA%\lueying\extensions\my-pack\
  manifest.json
  index.js
```

- 目录名必须等于清单里的 `id`,两个包不能共用同一个配置键。
- `index.js` 是单文件 IIFE,由你自己的 esbuild/tsc 打包。输入法不内置转译器,也不支持 `import`。
- 入口脚本在顶层同步调用一次 `lueying.register(handler)`;没有注册、注册两次、传非函数都会在加载期报错。

## 启用

打开设置页「候选 → 运行期脚本」,点「选择」挑一个包,保存后生效;也可以直接编辑 `%APPDATA%\lueying\config.ini`,写一行 `script=my-pack`(值是目录名)。空值、`off`、`none` 表示不启用。改完脚本不用重启,服务每秒按内容哈希检查一次热重载。

## manifest.json

```json
{
  "id": "my-pack",
  "name": "我的候选窗",
  "version": "1.0.0",
  "appearance": { "api": 1, "main": "index.js" }
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 小写字母、数字、`-`、`_`,最长 48 字节;必须等于目录名 |
| `name` | 是 | 显示名,1–80 字 |
| `version` | 是 | 扩展自己的版本号,只用于显示 |
| `appearance` | 是* | 外观组件;`api` 是场景协议版本,当前只支持 `1`,`main` 默认 `index.js` |
| `logic` / `panel` | 否 | 逻辑 WASM 与进程外能力组件,见[分层模型](layers.md) |
| `api` / `main` | 否 | 旧版顶层写法,等价于只有 `appearance`;不能和新组件混用 |
| `capabilities` | 否 | 旧字段,必须为空;新权限写在组件里 |

清单出现未知字段会被拒绝,避免按旧/新语义猜。*没有 `appearance` 也没有旧版 `api`/`main` 的包不是外观扩展。

## 场景状态

`handler(state)` 收到一个对象,字段固定:

| 字段 | 类型 | 说明 |
|---|---|---|
| `api` | number | 场景协议版本 |
| `composing` | boolean | 是否正在组合 |
| `page` / `pages` | number | 当前页(从 1 开始)与总页数 |
| `candidates` | number | 当前页候选数量 |
| `selected` | number | 当前页选中下标(从 0 开始) |
| `accent` | number | 主题色 `0xRRGGBB` |
| `dark` | boolean | 是否深色主题 |
| `high_contrast` | boolean | 系统是否开启高对比度 |
| `reduce_motion` | boolean | 系统是否开启「减少动态效果」 |
| `time` | number | 动画时钟(秒);减少动态效果时为 0 |
| `anim` | number | 入场/翻页进度 0–1,静止为 1 |
| `private` | boolean | 是否密码/隐私字段 |

状态里没有候选文字、拼音、宿主应用名与击键历史。`private` 为 true 时 handler 不会被调用,这次状态根本不会进 JS。

面板变化(按键、翻页、选中)时派发一次;`time` 与 `anim` 是派发时刻的时钟快照。候选窗里的动画由 SkSL 的 `u_time` 自己走,不靠 JS 每帧回调。

## 补丁

返回 `null` 或 `undefined` 表示这一帧不改;否则返回一个对象:

```js
{
  accent: 0x2f6fed,
  effects: {
    background: "uniform float2 u_size; half4 main(float2 p) { ... }",
    highlight: "...",
    menu: "..."
  }
}
```

- `accent` 是 24 位 `0xRRGGBB`,超出即整份补丁作废。它替换效果的 `u_accent` 与选中行底色;候选文字仍用主题里对比度有保证的颜色。
- 三个槽位与 [用户效果](../effects.md) 的契约相同;单份源码上限 64 KiB,空源码会被拒绝。
- 补丁没给的槽位继续用 `%APPDATA%\lueying\effects` 里的同名文件;想整块接管就三个都写。
- 补丁里的未知字段会被拒绝,整份补丁作废;不会只应用认得的那部分。
- 高对比度下 `effects` 被摘掉,只留 `accent`;效果本身的对比度遮罩仍由引擎负责。
- 新组合的第一帧仍走素色,补丁从第二帧起生效;候选一出现就可读。
- 补丁只改装饰层。拼音、候选、序号、注音永远由输入法绘制。

## 示例

```ts
type Scene = {
  accent: number;
  dark: boolean;
  high_contrast: boolean;
};

lueying.register((state: Scene) => {
  if (state.high_contrast) return null;
  return {
    accent: state.dark ? 0x8ab4f8 : 0x2f6fed,
    effects: {
      background: `
        uniform float2 u_size;
        uniform float u_anim;
        uniform float4 u_accent;
        half4 main(float2 p) {
          float edge = min(min(p.x, u_size.x - p.x), min(p.y, u_size.y - p.y));
          float a = smoothstep(0.0, 24.0, edge) * 0.5 * u_anim;
          return half4(u_accent.rgb * a, a);
        }
      `,
    },
  };
});
```

打包成单文件:

```sh
esbuild src/index.ts --bundle --format=iife --minify --outfile="%APPDATA%\lueying\extensions\my-pack\index.js"
```

## 限额与失效

| 项 | 上限 |
|---|---|
| 运行时内存 | 8 MiB |
| JS 栈 | 256 KiB |
| 初始化 | 100 ms |
| 单次派发 | 8 ms |
| 清单 / 入口脚本 / 单份效果源码 | 64 KiB / 512 KiB / 64 KiB |

- 初始化与每次派发都有指令时限,超时由 QuickJS 中断打断,不会拖住击键。
- 连续 3 次失败停用该包;热重载成功后计数清零。
- 热重载按内容哈希轮询,写坏时保留上一版可用的宿主;把文件改回来就恢复。
- 删除 `manifest.json` 视为卸载。

## 不支持

能力声明(文件、网络、候选注入)、`import`、异步 handler、定时器、DOM。清单里出现任何 `capabilities` 都会被拒绝;运行期不加载 DLL。
