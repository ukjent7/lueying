# SKILL: 写一个掠影运行期扩展

这个模板给 coding agent 用:把扩展做成一个可打包、可审计的 `lueying` 运行期脚本扩展。
交付物是一个目录(`manifest.json` + `index.js`),用户通过设置页「扩展 → 运行期脚本」
选择并同意权限后启用。

## 先确认边界

- 脚本是单文件 IIFE,不支持 `import`;先写 TypeScript,再用 esbuild/tsc 打包成一个文件。
- 不要尝试读候选文字、宿主应用名或键盘内容;场景里没有这些字段。
- 密码/隐私字段下宿主不会调用脚本,也不会执行命令。
- 联网、截屏、读剪贴板都属于高敏感能力:只在确实需要时声明,并在界面上向用户说明用途。
- 扩展随宿主进程运行,不能加载 DLL,也不能绕过宿主服务;所有外部能力走 `lueying.<service>.*`。

## 目录

```
my-pack/
  manifest.json
  index.js       # 打包产物
  README.md      # 给用户看:它做什么、需要什么权限、数据去哪
```

目录名必须等于 `manifest.json` 的 `id`(小写字母、数字、`-`、`_`,≤48 字节)。

## manifest.json

```json
{
  "id": "my-pack",
  "name": "我的扩展",
  "version": "1.0.0",
  "api": 2,
  "main": "index.js",
  "activate": {
    "commands": [{ "id": "run", "title": "执行我的命令", "when": "any" }]
  },
  "needs": [],
  "capabilities": [],
  "settings": []
}
```

- `activate` 至少要有 `triggers`、`hotkeys`、`commands` 之一。
- `needs` 只填用到的服务:`capture`、`ocr`、`clipboard`、`store`、`output`、`panel`、`hotkey`、`net`、`notify`;`net` 还要在清单顶层写 `network.domains` 白名单。
- `capabilities` 是权限声明,不是服务声明;读用户内容与外发同时出现时会触发强提示。
- 设置项最多 32 个,类型只有 `toggle`、`choice`、`secureText`;密钥不会写进 `config.toml`。

## index.js

```js
// 打包后仍是单文件 IIFE;下面是源码形态。
lueying.command("run", async () => {
  const { text } = await lueying.clipboard.read();
  await lueying.store.set({ key: "last", value: text.slice(0, 200) });
  await lueying.notify.show({ title: "我的扩展", body: "已处理剪贴板" });
  return { chars: text.length };
});
```

需要候选窗外观时注册场景处理:

```js
lueying.register((scene) => {
  if (scene.dark) {
    return { accent: 0x5aa9ff };
  }
  return {};
});
```

类型定义在同目录 [`lueying.d.ts`](lueying.d.ts);把 `lueying.d.ts` 加进 tsconfig 的
`include`,即可获得 `lueying` 全局类型。

## 开发与打包

```bash
# 用 esbuild 把 TypeScript 打成单文件
npx esbuild src/index.ts --bundle --format=iife --outfile=index.js

# 本地检查并安装到当前用户扩展目录
lueying ext pack my-pack
lueying ext install my-pack.luext
lueying ext reload
lueying ext logs
```

`lueying ext replay` 可以在 Linux 无头环境回放按键与命令,输出确定性候选报告;
提交前至少跑一次,并把报告贴到 PR/说明里。

## 验收清单

- [ ] 目录名 = `id`,`manifest.json` 能被 `lueying ext pack` 解析。
- [ ] 只声明实际使用的 `needs` 与 `capabilities`。
- [ ] 没有 `eval`、动态 `import`、绕过 `lueying.net` 的网络直连或读取候选文字。
- [ ] 命令在 `activate.commands` 里有标题与 `when`。
- [ ] 设置项有默认值,密钥项使用 `secureText`。
- [ ] 用户文档写清数据用途、存储位置与卸载后是否保留。
- [ ] 在设置页启用、同意权限、执行命令、关闭扩展各走一遍。

## 发布

优先让扩展留在用户自己的目录;需要公开分发时,把源码与打包步骤一起发布,
让用户能复现 `index.js`。不要把密钥、令牌或本机路径写进包。
