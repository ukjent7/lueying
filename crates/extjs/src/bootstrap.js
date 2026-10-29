// 扩展引导:用户脚本只拿到 lueying.register、lueying.command 与 lueying.timeout,
// 派发入口由这里封死。__LUEYING_API__ 在加载时由 Rust 侧替换成 SCENE_API。
// 所有用到的内建函数在用户脚本运行前就存进闭包,脚本改全局对象也换不掉。
(function () {
  "use strict";
  var parse = JSON.parse;
  var stringify = JSON.stringify;
  var toText = String;
  var keys = Object.keys;
  var PromiseCtor = Promise;
  var handler = null;
  var commands = Object.create(null);
  var timers = [];
  var pending = Object.create(null);
  var ready = [];
  var pending_seq = 0;
  var clock = 0;

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
    // 宿主时钟上的延时 Promise;等待不占脚本预算,由宿主 tick 推进。
    timeout: function (ms) {
      var delay = +ms;
      if (!(delay >= 0)) {
        delay = 0;
      }
      return new PromiseCtor(function (resolve) {
        timers.push({ at: clock + delay, resolve: resolve });
      });
    },
  };
  Object.freeze(api);
  Object.defineProperty(globalThis, "lueying", {
    value: api,
    writable: false,
    enumerable: false,
    configurable: false,
  });

  function track(promise, token) {
    pending[token] = true;
    promise.then(
      function (value) {
        ready.push({
          token: token,
          ok: true,
          value: value === undefined ? null : stringify(value),
        });
        delete pending[token];
      },
      function (e) {
        ready.push({ token: token, ok: false, error: describe(e) });
        delete pending[token];
      }
    );
  }

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
        if (
          result !== null &&
          typeof result === "object" &&
          typeof result.then === "function"
        ) {
          var token = ++pending_seq;
          track(result, token);
          return "__pending:" + token;
        }
        if (result === undefined) {
          return "";
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

  // 推进宿主时钟,兑现到点的定时器;宿主每次泵任务前调用。
  Object.defineProperty(globalThis, "__lueying_tick", {
    value: function (now) {
      clock = now;
      var due = [];
      var left = [];
      for (var i = 0; i < timers.length; i++) {
        if (timers[i].at <= now) {
          due.push(timers[i]);
        } else {
          left.push(timers[i]);
        }
      }
      timers = left;
      for (var j = 0; j < due.length; j++) {
        due[j].resolve(undefined);
      }
    },
    writable: false,
    enumerable: false,
    configurable: false,
  });

  // 取走已就绪的任务结果;宿主在跑完作业队列后调用。
  Object.defineProperty(globalThis, "__lueying_take_ready", {
    value: function () {
      var out = ready;
      ready = [];
      return stringify(out);
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
