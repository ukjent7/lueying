# 多语言扩展

掠影扩展的契约是 [`lueying.wit`](../../crates/extwasm/lueying.wit) 里的 `lueying:ext@2.0.0`
世界:组件导出 `entry` 接口(`init`/`command`/`appearance`),按清单 `needs`
导入十二个宿主服务接口。契约是标准 WebAssembly 组件,任何能产出组件的
语言都能写扩展;这里列当前验证过的工具链与最小路径。

无论哪种语言,共同约束见[分层与沙箱](layers.md):组件 ≤ 16 MiB、内存
16 MiB、栈 256 KiB、燃料按初始化/命令/外观分段计量、连错三次熔断。宿主
给组件提供的 WASI 0.2 能力是全关的——没有文件、没有网络、没有环境变量、
没有标准流,只有时钟与随机;联网只能走 `network` 服务(清单白名单逐请求
强制)。组件不要导入 `wasi:http`,宿主不提供。

## Rust

主路径,SDK 与示例齐全,见 [wasm.md](wasm.md)。

## JavaScript(TypeScript)

工具链:`jco`(内嵌 componentize-js 的 SpiderMonkey 引擎)。

```sh
npm install -D @bytecodealliance/jco
```

写一个 ES 模块,按 `entry` 接口导出对象;宿主服务按 WIT 包路径导入
(`命名空间:包/接口@版本`):

```js
import { show } from "lueying:ext/notify@2.0.0";

export const entry = {
  init() {},

  command(id, args) {
    switch (id) {
      case "run":
        return JSON.stringify({ echo: args });
      default:
        throw new Error(`没有命令 ${id}`);
    }
  },

  appearance(_sceneJson) {
    return null;
  },
};
```

`result` 的错误分支用抛异常表达:抛出的 `Error` 文本就是 Err 载荷;
`option` 用 `null` 表示 none。构建(禁用 `http` 与 `fetch-event`,宿主
不提供 `wasi:http`):

```sh
npx jco componentize guest.js \
  --wit <掠影仓库>/crates/extwasm/lueying.wit \
  --world-name extension \
  --disable http fetch-event \
  --out extension.wasm
```

## Python

工具链:[`componentize-py`](https://github.com/bytecodealliance/componentize-py)。

```sh
pip install componentize-py
```

把 `lueying.wit` 放进项目的 `wit/` 目录,先生成绑定:

```sh
componentize-py --wit-path wit --world extension bindings .
```

`wit_world/exports` 里有 `Entry` 协议,实现它(`app.py`);方法与 WIT
一一对应,`result` 的错误分支按生成代码的类型标注处理:

```py
from wit_world import exports

class Entry(exports.Entry):
    def init(self) -> None:
        pass

    def command(self, id: str, args: str) -> str:
        return args

    def appearance(self, scene_json: str):
        return None
```

构建:

```sh
componentize-py --wit-path wit --world extension componentize app -o extension.wasm
```

## C / C++

工具链:[`wit-bindgen`](https://github.com/bytecodealliance/wit-bindgen) C
绑定 + [WASI SDK](https://github.com/webassembly/wasi-sdk)(wasm32-wasip2
直出组件,不用适配器)。

```sh
wit-bindgen c crates/extwasm/lueying.wit
```

生成 `lueying_ext.c`/`lueying_ext.h`/`lueying_ext_component_type.o`;在
`component.c` 里实现 `exports_lueying_ext_entry_init/command/appearance`
三个函数,然后:

```sh
/opt/wasi-sdk/bin/wasm32-wasip2-clang \
  -o extension.wasm -mexec-model=reactor \
  component.c lueying_ext.c lueying_ext_component_type.o
```

C 标准库的 `printf` 等会引入 WASI 导入,宿主已提供;但产物里别带
`wasi:http`。

## Go

工具链:[`componentize-go`](https://github.com/bytecodealliance/componentize-go)(Go 1.25+),
官方语言文档收录,`componentize-go --world extension bindings` 生成绑定后
`componentize-go build`。相对 Rust/JS/C 还年轻,遇到问题优先看其仓库示例。

## 打包与分发

组件就是包目录里的 `extension.wasm`,清单要求见[扩展总览](README.md);
`lueying ext pack` 打包,`lueying ext install` 安装,发布与收录见
[社区目录](catalog.md)。
