/* 掠影运行期扩展的 guest ABI(C 与 C++ 通用)。
 *
 * 扩展就是标准 Extism 插件:数据放在 extism 的内存里,命令按 id 导出。
 * Rust 侧有 lueying-extguest 便利封装,这里的声明与它等价;别的语言用
 * 官方 Extism PDK 也一样能跑。
 *
 * 编译:
 *   clang --target=wasm32-unknown-unknown -nostdlib -O2 \
 *         -Wl,--no-entry -Wl,--export-memory \
 *         -o extension.wasm guest.c
 *
 * 清单 api 字段必须等于 LUEYING_EXT_API,ABI 变了宿主会拒绝加载。
 */
#ifndef LUEYING_EXT_H
#define LUEYING_EXT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* 清单 api 字段的取值。 */
#define LUEYING_EXT_API 4

/* extism 内核(模块 "extism:host/env")。
 * guest 的数据都在 extism 的内存里:分配一块、写进去、交给宿主。 */

__attribute__((import_module("extism:host/env"), import_name("alloc")))
uint64_t extism_alloc(uint64_t len);

__attribute__((import_module("extism:host/env"), import_name("free")))
void extism_free(uint64_t offset);

__attribute__((import_module("extism:host/env"), import_name("length")))
uint64_t extism_length(uint64_t offset);

__attribute__((import_module("extism:host/env"), import_name("load_u8")))
uint8_t extism_load_u8(uint64_t offset);

__attribute__((import_module("extism:host/env"), import_name("store_u8")))
void extism_store_u8(uint64_t offset, uint8_t value);

__attribute__((import_module("extism:host/env"), import_name("input_length")))
uint64_t extism_input_length(void);

__attribute__((import_module("extism:host/env"), import_name("input_load_u8")))
uint8_t extism_input_load_u8(uint64_t index);

__attribute__((import_module("extism:host/env"), import_name("output_set")))
void extism_output_set(uint64_t offset, uint64_t len);

__attribute__((import_module("extism:host/env"), import_name("error_set")))
void extism_error_set(uint64_t offset);

/* 宿主服务(模块 "extism:host/user")。
 * 请求是一段 JSON,回一段 JSON;返回值是 extism 内存里那段响应的偏移。
 * 清单 needs 里没声明的服务不会被链接,用到就在实例化时失败。 */
#define LUEYING_SERVICE(name)                                                 \
  __attribute__((import_module("extism:host/user"),                            \
                 import_name("lueying_" #name)))                              \
  uint64_t lueying_##name(uint64_t request)

LUEYING_SERVICE(capture);
LUEYING_SERVICE(ocr);
LUEYING_SERVICE(output);
LUEYING_SERVICE(panel);
LUEYING_SERVICE(clipboard);
LUEYING_SERVICE(store);
LUEYING_SERVICE(hotkey);
LUEYING_SERVICE(notify);
LUEYING_SERVICE(network);
LUEYING_SERVICE(context);

/* 可取消的等待(毫秒);宿主卸载扩展时返回 -1。 */
__attribute__((import_module("extism:host/user"), import_name("lueying_timeout")))
int32_t lueying_timeout(int64_t ms);

/* guest 导出:宿主按清单与调用点取用。
 * 下面的宏贴在函数定义前面,把符号名改成宿主要的那个。 */

/* 初始化;返回 0 成功。可不实现。 */
#define LUEYING_EXPORT_INIT                                                   \
  __attribute__((export_name("lueying_init"))) int32_t lueying_init(void)

/* 命令入口:导出名就是命令 id,清单里的命令 id 要与它一致。
 * 宿主把参数 JSON 作为 extism 输入传进来;结果用 lueying_output 交回。 */
#define LUEYING_EXPORT_COMMAND(name)                                          \
  __attribute__((export_name(#name))) int32_t name(void)

/* 外观帧:清单 appearance 为 true 时必须实现,结果用 lueying_output 交回。 */
#define LUEYING_EXPORT_APPEARANCE                                             \
  __attribute__((export_name("lueying_appearance"))) int32_t lueying_appearance( \
      void)

/* 把一段字节交给宿主当结果。 */
static inline int32_t lueying_output(const uint8_t *bytes, uint64_t len) {
  uint64_t offset = extism_alloc(len);
  for (uint64_t i = 0; i < len; i++) {
    extism_store_u8(offset + i, bytes[i]);
  }
  extism_output_set(offset, len);
  return 0;
}

/* 把一段文本交给宿主当错误;宿主把它当成命令失败的原因。 */
static inline int32_t lueying_error(const char *text) {
  uint64_t len = 0;
  while (text[len] != '\0') {
    len++;
  }
  uint64_t offset = extism_alloc(len);
  for (uint64_t i = 0; i < len; i++) {
    extism_store_u8(offset + i, (uint8_t)text[i]);
  }
  extism_error_set(offset);
  return -1;
}

#ifdef __cplusplus
}
#endif

#endif /* LUEYING_EXT_H */
