# 给 coding agent 的扩展编写指南

这份文档面向用 AI coding agent(或新接手的开发者)写掠影扩展的人:在
Linux 无头环境里走完「建包 → 改 → 装 → 回放验证」的整个回路,全程不需要
Windows。扩展的清单格式、脚本 API、服务调用与限额见
[script.md](script.md);分层与权限模型见 [layers.md](layers.md);可直接
复制给 agent 的模板见 [SKILL.md](SKILL.md),TypeScript 声明见
[lueying.d.ts](lueying.d.ts)。

## 开发回路

`lueying` 命令行覆盖扩展的完整生命周期,Windows 与 Linux 上行为一致:

```text
lueying ext new demo            # 建骨架 demo/manifest.json + demo/index.js
…编辑 demo/ 里的清单与脚本…
lueying ext pack demo           # 打包成 demo.luext(纯文本,可 diff、可入库)
lueying ext install demo.luext  # 装进扩展目录;已有包要加 --force
lueying ext reload              # 让常驻服务重读配置与扩展(服务未运行时报错)
lueying ext replay 脚本.txt     # 无头回放:按键序列进,候选列表出
lueying ext logs --tail 50      # 看服务日志末尾
lueying ext top                 # 看运行指标:内存、峰值、超限、停用原因
```

`new` 生成的骨架自带一条 `hello` 命令和一个声明式设置项,装上就能在设置页
看到,用它当最小样例改。

## 包的规则

- 目录名必须等于清单 `id`;`id` 只允许小写字母、数字、`-`、`_`,以字母或
  数字开头,最长 48 字节。
- 打包只收文本文件:`.js` `.json` `.txt` `.sksl` `.md`,必须是 UTF-8。发现
  二进制文件 `pack` 直接报错,不会静默丢文件。
- `.luext` 是行数声明的文本块格式(`### lueying-pack v1`),与配置备份同一
  格式,可以直接提交进仓库、走 code review。
- `install` 默认不覆盖已有包;覆盖要 `--force`。装完会再校验一次清单。

## 无头回放(replay)

回放把一份按键脚本喂给与服务端同一套引擎,输出每一步的上屏、组合与候选,
用来验证拼音行为、触发词与动作候选,不需要窗口与 TSF。

```text
lueying ext replay script.txt              # 显式给脚本
echo "nihao" | lueying ext replay          # 不给脚本就读标准输入
lueying ext replay --trigger ocr=demo.run script.txt
lueying ext replay --config /tmp/config.toml script.txt
```

按键脚本每行一个阶段:

- `#` 开头是注释,空行忽略;一行内字母、数字逐键输入。
- `<名字>` 是功能键:`space` `enter` `esc` `back` `delete` `left` `right`
  `up` `down` `home` `end` `pageup` `pagedown` `shift`。
- `;` `'` `,` `.` `/` `` ` `` 作标点键;`'` 是隔音符。
- 看不懂的字符会带行号报错,脚本里不要写中文或不可见字符。

输出格式:

```text
> nihao
  组合 nihao · 候选 你好 / 你家 / nihao
> <space>
  上屏 你好
  组合 (无)
```

触发词与动作候选按服务端同一条路径注入:整串输入(去隔音符)命中触发表,
且命令标题能在已安装包的清单里找到,动作候选才出现;它默认排第二位,数字
键执行并回显 `动作 <命令 id>`,空格照旧上屏中文首选,Enter 照旧上屏原文。
触发表来自 config.toml 的 `[triggers]`(`词 = "扩展.命令"`),`--trigger`
旗标叠加在其上、同名时优先。

回放是确定性的:内置迷你词库、全新用户词库、默认全拼、联想关闭。同一个脚
本在任何机器上输出一致,适合直接作为扩展的回归用例放进 PR 描述或 issue。

## 声明式设置

清单里的 `settings` 数组声明开关与单选(`toggle`/`choice`,各带 `default`),
宿主把它们渲染进设置页「扩展 · <名称>」分组,值存在 config.toml 的
`[ext.<id>]`。宿主按清单校验取值:未知键或类型不符整次保存失败,配置不改。
外部程序经命名管道用 `Request::Ext { id, method, params }` 读写同一份值
(`settings.get` / `settings.set`,见 [script.md](script.md));脚本侧读取
设置值的能力在后续任务接入,当前脚本不要依赖它。

## 收尾前自查

1. `lueying ext pack` 打包成功、`install` 后 `replay` 能跑通主路径。
2. `replay` 覆盖边界:空组合、非法触发词、命令不存在(标题表里没有时动作
   候选不出现)、隐私字段语义(宿主在隐私字段整个停用扩展层)。
3. `lueying ext top` 里没有异常的内存或超时;`logs` 里没有扩展报错。
4. 清单声明与实际一致:`commands` 只列真实现的命令,`needs` 只列真调用的
   服务;未声明的调用在宿主当场被拒。
5. CI 会跑 fmt、clippy、测试与 E2E;本仓库不在开发机装 Rust 工具链,提交
   后让 CI 裁决,失败按日志修。

## 边界

- 扩展跑在 QuickJS 沙箱:内存 8 MiB、栈 256 KiB、初始化 100 ms、单次派发
  8 ms,连续 3 次失败停用。等原生服务返回的 Promise 不占派发预算,只有真
  正跑在 JS 里的作业步受预算限制。
- 没有 `import`、定时器、DOM;联网需要 `network.domains` 白名单并经独立代理进程,不要把网络请求放进输入
  热路径。
- 隐私字段(密码框等)不调用脚本、不注入动作候选、不派发热键,这是宿主强
  制的,脚本无法绕过。
