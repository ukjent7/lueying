/* 用 C 写运行期扩展的最小例子:命令回一句固定 JSON。
 *
 * 编译(CI 与本地都用这一条):
 *   clang --target=wasm32-unknown-unknown -nostdlib -O2 \
 *         -I crates/extguest/include \
 *         -Wl,--no-entry -Wl,--export-memory \
 *         -o target/c-guest/example.wasm crates/extguest/c/example.c
 *
 * 导出名就是命令 id;宿主按清单里的命令 id 调用同名导出,参数 JSON 作为
 * extism 输入传进来。真正的扩展把命令解析与服务调用写进自己的导出即可。 */
#include "lueying_ext.h"

LUEYING_EXPORT_COMMAND(run) {
  static const char reply[] = "{\"lang\":\"c\"}";
  return lueying_output((const uint8_t *)reply, (uint64_t)(sizeof(reply) - 1));
}
