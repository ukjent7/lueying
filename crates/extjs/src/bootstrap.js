// 扩展引导:用户脚本只拿到 lueying.register 与 lueying.command,派发入口由这里封死。
// __LUEYING_API__ 在加载时由 Rust 侧替换成 SCENE_API。
// 所有用到的内建函数在用户脚本运行前就存进闭包,脚本改全局对象也换不掉。
(function () {
  "use strict";
  var parse = JSON.parse;
  var stringify = JSON.stringify;
  var toText = String;
  var keys = Object.keys;
  var handler = null;
  var commands = Object.create(null);

  function describe(e) {
    // QuickJS 的 stack 只有调用栈,没有错误文本;两段都带上,日志里才看得到
    // 「out of memory」「Error: boom」这类原因。
    var message = toText(e);
    var stack = e && e.stack ? toText(e.stack) : "";
    return stack ? message + " | " + stack : message;
  }

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
    command: function (name, fn) {
      if (typeof name !== "string" || !name) {
        throw new TypeError("lueying.command 需要命令名");
      }
      if (typeof fn !== "function") {
        throw new TypeError("lueying.command 需要函数");
      }
      if (commands[name] !== undefined) {
        throw new Error("命令重复注册: " + name);
      }
      commands[name] = fn;
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
        return "";
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
        throw describe(e);
      }
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });

  Object.defineProperty(globalThis, "__lueying_invoke", {
    value: function (name, json) {
      var fn = commands[name];
      if (fn === undefined) {
        throw new Error("命令未注册: " + name);
      }
      try {
        var args = json === "" ? undefined : parse(json);
        var result = fn(args);
        if (result === undefined) {
          return "";
        }
        if (typeof result === "object" && result !== null && typeof result.then === "function") {
          throw new TypeError("命令必须是同步函数");
        }
        return stringify(result);
      } catch (e) {
        throw describe(e);
      }
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });

  Object.defineProperty(globalThis, "__lueying_has_command", {
    value: function (name) {
      return commands[name] !== undefined;
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });

  Object.defineProperty(globalThis, "__lueying_ready", {
    value: function () {
      return handler !== null || keys(commands).length > 0;
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });
})();
