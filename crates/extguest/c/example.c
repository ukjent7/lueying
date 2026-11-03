/* 用 C 写运行期扩展的最小例子:命令回一句固定 JSON。
 *
 * 编译(CI 与本地都用这一条):
 *   clang --target=wasm32-unknown-unknown -nostdlib -O2 \
 *         -I crates/extguest/include \
 *         -Wl,--no-entry -Wl,--export-memory \
 *         -o target/c-guest/example.wasm crates/extguest/c/example.c
 *
 * 宿主要的导出都在这里;真正的扩展把命令解析与服务调用写进
 * lueying_command 即可。 */
#include "lueying_ext.h"

/* 服务结果缓冲区:宿主把 JSON 信封写在这里,guest 按返回的字节数读。
 * 指针要稳定,所以用静态数组而不是栈上 buffer。 */
static uint8_t result[65536];

/* 宿主写输入前要一块 guest 内存;这里用一块静态 arena 顶替分配器。 */
static uint8_t arena[65536];
static uint32_t arena_used;

LUEYING_EXPORT_RESULT_PTR { return (uint32_t)(uintptr_t)result; }
LUEYING_EXPORT_RESULT_CAP { return (uint32_t)sizeof(result); }

LUEYING_EXPORT_ALLOC {
  uint32_t offset = arena_used;
  arena_used += len;
  return (uint32_t)(uintptr_t)(arena + offset);
}

LUEYING_EXPORT_FREE {
  (void)ptr;
  (void)len;
}

LUEYING_EXPORT_INIT { return 0; }

LUEYING_EXPORT_COMMAND {
  (void)id;
  (void)id_len;
  (void)args;
  (void)args_len;
  static const char reply[] = "{\"lang\":\"c\"}";
  lueying_emit((const uint8_t *)reply, (uint32_t)(sizeof(reply) - 1), 1);
  return 0;
}
