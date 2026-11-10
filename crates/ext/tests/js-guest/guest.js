// 行为测试客人(JS 版,componentize-qjs 工具链):命令回显、报错、通知;
// 供扩展宿主验证非 Rust 工具链的端到端路径(CI 构建后经环境变量
// 分发,见 crates/ext/src/host/test_guest.rs)。
//
// WIT 的 result 错误分支用抛值表达:抛出的字符串就是 Err 的载荷;
// option 的 none 用 null 表示。服务导入按 WIT 包路径取:
// `命名空间:包/接口@版本`。
import { show } from "lueying:ext/notify@2.0.0";

export const entry = {
  init() {},

  command(id, args) {
    switch (id) {
      // 回显:参数原样返回,命令与热重载断言用。
      case "run":
      case "echo":
        return args;
      // 带着参数文本报错,错误回传断言用。
      case "fail":
        throw args;
      // 调一次宿主通知服务;needs 门控与跨语言服务桥接断言用。
      case "notify":
        show({ title: "JS 测试客人", body: args });
        return JSON.stringify({ notified: true });
      default:
        throw `没有命令 ${id}`;
    }
  },

  // 不改外观:返回 null 即 WIT 的 none 分支。
  appearance(_sceneJson) {
    return null;
  },
};
