# 运行期脚本扩展

候选窗的外观可以交给一段 JS 决定,扩展也可以提供命令。扩展只改主题色与三个 SkSL 槽位,看不到候选文字,也拿不到宿主应用名。

> 状态:脚本宿主、多扩展有序加载、命令声明与 Scope 回收已就绪;Linux E2E 覆盖坏清单、坏补丁、死循环、内存暴涨、隐私、热重载与卸载回收。截图、OCR、剪贴板等服务缝与异步步进在后续阶段。

## 包结构

```
%APPDATA%\lueying\extensions\my-pack\
  manifest.json
  index.js
```

- 目录名必须等于清单里的 `id`,两个包不能共用同一个配置键。
- `index.js` 是单文件 IIFE,由你自己的 esbuild/tsc 打包。输入法不内置转译器,也不支持 `import`。
- 入口脚本在顶层同步注册至少一个派发入口:`lueying.register(handler)` 或 `lueying.command(name, fn)`。没有注册、重复注册、传非函数都会在加载期报错。

## 启用

打开设置页「候选 → 运行期脚本」,点「选择」挑一个包,保存后生效。多扩展直接编辑 `%APPDATA%\lueying\config.ini`,写一行逗号分隔的列表:

```ini
extensions=shot-ocr,clipboard-history
```

顺序即组合顺序:后面的扩展返回的非空补丁字段覆盖前面的。空值、`off`、`none` 表示不启用;旧键 `script=my-pack` 仍可读,等价于只有一个扩展。改完脚本不用重启,服务每秒按内容哈希检查一次热重载。

## manifest.json

API 2 的脚本清单:

```json
{
  "id": "my-pack",
  "name": "我的扩展",
  "version": "1.0.0",
  "api": 2,
  "main": "index.js",
  "activate": {
    "triggers": ["my"],
    "hotkeys": ["Ctrl+Alt+M"],
    "commands": [
      { "id": "run", "title": "执行我的命令", "when": "any" }
    ]
  },
  "needs": ["notify"],
  "capabilities": []
}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 小写字母、数字、`-`、`_`,最长 48 字节;必须等于目录名 |
| `name` | 是 | 显示名,1–80 字 |
| `version` | 是 | 扩展自己的版本号,只用于显示 |
| `api` | 是 | 清单 API;当前支持 `1`(旧版外观)与 `2`(脚本层) |
| `main` | 否 | 脚本入口,包内裸文件名,默认 `index.js` |
| `activate` | 否 | 激活声明;`triggers`、`hotkeys`、`commands` 至少一项 |
| `needs` | 否 | 需要的宿主服务:capture、ocr、clipboard、panel、output、hotkey、net、store、notify |
| `capabilities` | 否 | 申请的权限;API 1 必须为空 |

旧版外观包继续可用:

```json
{
  "id": "my-pack",
  "name": "我的候选窗",
  "version": "1.0.0",
  "appearance": { "api": 1, "main": "index.js" }
}
```

顶层 `{ "api": 1, "main": "index.js" }` 等价于只有 `appearance` 组件。API 2 不能和 `appearance`/`logic`/`panel` 混用。清单出现未知字段会被拒绝,避免按旧/新语义猜。

## 命令

命令在清单里声明,在 JS 里实现。完整 id 是 `<扩展 id>.<命令 id>`,例如 `my-pack.run`:

```js
lueying.command("run", (args) => {
  return { ok: true, received: args ?? null };
});
```

- 只能执行清单里声明过的命令;JS 里注册了未声明的名字也调不到。
- 参数与返回值都走 JSON;返回 `undefined` 表示没有返回值。
- 命令可以返回 Promise:等待不占脚本预算,由宿主时钟推进;每个作业步仍受
  8 ms 限,反复重新入队的任务每轮泵至多 256 步。`lueying.timeout(ms)` 返回
  宿主时钟上的延时 Promise,是等待的最小原语;原生服务(OCR、截图)的等待
  也走同一条预算规则。作业被预算打断时该命令按失败丢弃(引擎层面拿不到
  它的 Promise 结算),挂起记账清零,扩展内其余任务照常推进。
- 命令失败与派发失败共用连续失败计数,连续 3 次停用该扩展。
- `when` 可选 `any`、`editable`、`composing`,由宿主在绑定时过滤。

全局热键由宿主的用户绑定表管理,扩展清单里的 `activate.hotkeys` 只是建议。
`config.ini` 写 `bindings=Ctrl+Alt+O=my-pack.run,...` 把组合串映射到完整
命令 id;宿主强制至少两个修饰键、拒绝系统保留组合,同组合冲突时报已绑定的
命令。命中后按命令的 `when` 与当前输入状态过滤,再执行命令并写日志。Scope
随扩展卸载整体回收。异步示例:

```js
lueying.command("later", async (args) => {
  await lueying.timeout(args.ms);
  return { done: true };
});
```

宿主拿到 `Invoke::Pending` 后按宿主时钟泵作业队列;等待期间扩展照常参与空闲卸载计时。

命令面板由总热键唤出:`config.ini` 的 `panel_hotkey=Ctrl+Alt+Shift+Space`
(写 `off` 关闭)。面板自带输入框,不经过 TSF 会话:输入拼音时用同一台引擎
的候选匹配命令标题,拉丁标题按子串直接匹配;上下键选择、Enter 执行、Esc
取消,点行也能执行。唤出前记录前台窗口,面板关闭后归还焦点;面板是独立的
可获焦窗口,与无焦点候选窗分开。面板列出全部已声明命令,显式唤出不按
`when` 过滤。没有启用任何扩展时面板照常打开,只是没有命令可执行。

输入内触发(动作候选)用同一份用户绑定表的另一种行:
`triggers=bc=my-pack.run,...`。整串输入与触发词完全一致时,命令以候选形式
出现,带「命令」提示,默认排第 2 位,不做前缀匹配;数字键、方向键+空格或
点选执行命令,不上屏文字,组合清空。空格与 Enter 照旧上屏文字:原文候选
始终可达(缺了就补在页尾,带「原文」提示)。用户连续两次选字面原文后该
触发词降频(记同键负分,按周衰减),用既有置顶机制置顶后动作候选排首位。
触发词由用户配置决定,扩展清单只能给建议。破坏性命令的二次确认随命令清单
的标注一起接入,当前所有动作命令都按普通命令执行。

## 服务调用

清单 `needs` 里声明过的服务经 `lueying.<service>.<方法>(args)` 调用,返回
Promise;结果与错误都由宿主提供者异步回传。没声明的服务当场拒绝,声明了但
宿主没装提供者同样报错。接口与载荷以 `crates/extsvc/lueying.wit` 为准,
v1 开放:

| 服务 | 方法 | 说明 |
|---|---|---|
| capture | `shot({region?})` | 截屏,返回 `{file, width, height}` 临时位图路径 |
| ocr | `recognize({file, lang?})` | 文字识别,返回 `{text, lang}` |
| output | `deliver({text})` | 把文本送进当前输入焦点,返回 `{delivered}` |
| panel | `show({title, items})` | 宿主面板挑选,返回 `{selected}` |
| hotkey | `register({id, modifiers, key})` / `unregister({id})` / `press({id})` | 全局热键,冲突报错;`press` 等一次触发,扩展在 async 循环里反复调用 |
| notify | `show({title, body})` | 系统通知 |

剪贴板、网络与存储在 v1 没有开放接口;声明了也调不到。

```js
lueying.command("shot-ocr", async () => {
  const shot = await lueying.capture.shot({});
  const ocr = await lueying.ocr.recognize({ file: shot.file });
  await lueying.output.deliver({ text: ocr.text });
  return { chars: ocr.text.length };
});
```

等待提供者不占脚本预算;服务错误文本走 Promise 拒绝,计入连续失败计数。

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
| 单次派发 / 命令 | 8 ms |
| 清单 / 入口脚本 / 单份效果源码 | 64 KiB / 512 KiB / 64 KiB |
| 命令包空闲卸载 | 30 s |

- 初始化与每次派发都有指令时限,超时由 QuickJS 中断打断,不会拖住击键。
- 连续 3 次失败停用该扩展;热重载成功后计数清零。
- 热重载按内容哈希轮询,写坏时保留上一版可用的宿主;把文件改回来就恢复。
- 删除 `manifest.json` 视为卸载;Scope 里的命令、触发词与热键一起消失。

## 不支持

`import`、定时器(`setTimeout` 用 `lueying.timeout` 代替)、DOM、运行期加载 DLL。网络、剪贴板、截图、OCR 等能力要等宿主服务缝接线,并且只按清单声明授权。外观 handler 保持同步;只有命令可以返回 Promise。
