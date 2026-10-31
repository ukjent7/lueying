// 扩展引导:用户脚本只拿到 lueying.register、lueying.command、lueying.timeout
// 与声明过的 lueying.<service>.*,派发入口由这里封死。__LUEYING_API__ 在加载时
// 由 Rust 侧替换成 SCENE_API,__LUEYING_NEEDS__ 替换成清单 needs 的服务名数组。
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
  var needs = __LUEYING_NEEDS__;
  var services_pending = Object.create(null);

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
    // 服务缝:方法都在,调用面按 needs 门控;接口见 extsvc 的 lueying.wit。
    capture: { shot: serviceFn("lueying.capture.shot", "capture", "shot") },
    ocr: { recognize: serviceFn("lueying.ocr.recognize", "ocr", "recognize") },
    clipboard: {
      read: serviceFn("lueying.clipboard.read", "clipboard", "read"),
      write: serviceFn("lueying.clipboard.write", "clipboard", "write"),
    },
    store: {
      get: serviceFn("lueying.store.get", "store", "get"),
      set: serviceFn("lueying.store.set", "store", "set"),
      remove: serviceFn("lueying.store.remove", "store", "remove"),
      list: serviceFn("lueying.store.list", "store", "list"),
    },
    output: { deliver: serviceFn("lueying.output.deliver", "output", "deliver") },
    panel: { show: serviceFn("lueying.panel.show", "panel", "show") },
    net: { fetch: serviceFn("lueying.net.fetch", "net", "fetch") },
    hotkey: {
      register: serviceFn("lueying.hotkey.register", "hotkey", "register"),
      unregister: serviceFn(
        "lueying.hotkey.unregister",
        "hotkey",
        "unregister"
      ),
      press: serviceFn("lueying.hotkey.press", "hotkey", "press"),
    },
    notify: { show: serviceFn("lueying.notify.show", "notify", "show") },
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

  // 服务调用:发起时把 resolve/reject 存进 services_pending,宿主拿到
  // 提供者的完成结果后经 __lueying_service_resolve 兑现。参数是对象或空。
  function servicePromise(service, method, args) {
    return new PromiseCtor(function (resolve, reject) {
      var token = ++pending_seq;
      var json = args === undefined || args === null ? "" : stringify(args);
      var out;
      try {
        out = __lueying_service_call(service, method, json, token);
      } catch (e) {
        reject(describe(e));
        return;
      }
      if (out.slice(0, 8) === "__error:") {
        reject(out.slice(8));
        return;
      }
      services_pending[token] = { resolve: resolve, reject: reject };
    });
  }

  // 声明过的服务才有调用面:没在 needs 里声明的当场拒绝,错误文本直说。
  function serviceFn(label, service, method) {
    return function (args) {
      if (needs.indexOf(service) < 0) {
        return PromiseCtor.reject(label + " 未在 needs 声明");
      }
      return servicePromise(service, method, args);
    };
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

  // 兑现一次服务调用:宿主从完成队列抽到结果后调用;找不到等待中的
  // Promise 就丢弃(重复完成或调用方没等结果)。
  Object.defineProperty(globalThis, "__lueying_service_resolve", {
    value: function (token, ok, value) {
      var entry = services_pending[token];
      if (entry === undefined) {
        return;
      }
      delete services_pending[token];
      if (ok) {
        entry.resolve(value === "" ? undefined : parse(value));
      } else {
        entry.reject(value);
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
