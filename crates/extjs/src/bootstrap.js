// 扩展引导:用户脚本只拿到 lueying.register,派发入口由这里封死。
// __LUEYING_API__ 在加载时由 Rust 侧替换成 SCENE_API。
// 所有用到的内建函数在用户脚本运行前就存进闭包,脚本改全局对象也换不掉。
(function () {
  "use strict";
  var parse = JSON.parse;
  var stringify = JSON.stringify;
  var toText = String;
  var handler = null;

  var api = {
    api: __LUEYING_API__,
    register: function (fn) {
      if (typeof fn !== "function") {
        throw new TypeError("lueying.register 需要函数");
      }
      if (handler !== null) {
        throw new Error("lueying.register 只能调用一次");
      }
      handler = fn;
    },
  };
  Object.freeze(api);
  Object.defineProperty(globalThis, "lueying", {
    value: api,
    writable: false,
    enumerable: false,
    configurable: false,
  });

  Object.defineProperty(globalThis, "__lueying_dispatch", {
    value: function (json) {
      if (handler === null) {
        throw "扩展没有注册 handler";
      }
      try {
        var patch = handler(parse(json));
        if (patch === null || patch === undefined) {
          return "";
        }
        if (typeof patch !== "object") {
          throw new TypeError("handler 必须返回对象或 null");
        }
        if (typeof patch.then === "function") {
          throw new TypeError("handler 必须是同步函数");
        }
        return stringify(patch);
      } catch (e) {
        // QuickJS 的 stack 只有调用栈,没有错误文本;两段都带上,日志里才看得到
        // 「out of memory」「Error: boom」这类原因。
        var message = toText(e);
        var stack = e && e.stack ? toText(e.stack) : "";
        throw stack ? message + " | " + stack : message;
      }
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });

  Object.defineProperty(globalThis, "__lueying_ready", {
    value: function () {
      return handler !== null;
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });
})();
