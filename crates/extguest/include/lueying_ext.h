/* 掠影运行期扩展的 guest ABI(C 与 C++ 通用)。
 *
 * 契约只有两条:guest 从模块 "lueying" 导入宿主服务,guest 导出 lueying_*
 * 符号。宿主用什么 JIT 与这份契约无关,任何能产出 wasm32-unknown-unknown
 * 模块的工具链都能实现它;Rust 侧有 lueying-extguest 便利封装,这里的声明
 * 与它等价。
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
#define LUEYING_EXT_API 3

/* 宿主导入(模块 "lueying")。 */

/* 交回命令或外观的最终结果;ok 非 0 表示成功,0 表示失败并附错误文本。 */
__attribute__((import_module("lueying"), import_name("emit")))
void lueying_emit(const uint8_t *bytes, uint32_t len, int32_t ok);

/* 可取消的等待(毫秒);宿主卸载扩展时返回 -1。 */
__attribute__((import_module("lueying"), import_name("timeout")))
int32_t lueying_timeout(int64_t ms);

/* 服务调用:方法名与参数都指向 guest 线性内存。
 * 返回值是写进结果缓冲区的字节数,-1 表示宿主拒绝这次调用。
 * 结果是一个 JSON 信封:成功 {"ok":true,"value":...},失败
 * {"ok":false,"error":"..."}。 */
#define LUEYING_SERVICE(name)                                                 \
  __attribute__((import_module("lueying"), import_name(#name)))               \
  int32_t lueying_##name(const uint8_t *method, uint32_t method_len,          \
                         const uint8_t *args, uint32_t args_len)

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

/* guest 导出:宿主按清单与调用点取用。
 * 下面的宏贴在函数定义前面,把符号名改成宿主要的那个。 */

/* 初始化;返回 0 成功,非 0 时经 lueying_emit 交错误文本。可不实现。 */
#define LUEYING_EXPORT_INIT                                                   \
  __attribute__((export_name("lueying_init"))) int32_t lueying_init(void)

/* 命令入口:id 与 args 是宿主写进 guest 内存的 JSON 文本;返回 0 成功。 */
#define LUEYING_EXPORT_COMMAND                                                \
  __attribute__((export_name("lueying_command"))) int32_t lueying_command(    \
      const uint8_t *id, uint32_t id_len, const uint8_t *args,                \
      uint32_t args_len)

/* 外观帧:清单 appearance 为 true 时必须实现,返回 0 成功。 */
#define LUEYING_EXPORT_APPEARANCE                                             \
  __attribute__((export_name("lueying_appearance"))) int32_t lueying_appearance( \
      const uint8_t *scene, uint32_t scene_len)

/* 宿主写输入前问 guest 要一块内存,用完交回。 */
#define LUEYING_EXPORT_ALLOC                                                  \
  __attribute__((export_name("lueying_alloc"))) uint32_t lueying_alloc(        \
      uint32_t len)

#define LUEYING_EXPORT_FREE                                                  \
  __attribute__((export_name("lueying_free"))) void lueying_free(uint32_t ptr, \
                                                                 uint32_t len)

/* 服务结果缓冲区:宿主把 JSON 信封写在这里,guest 按返回的字节数读。
 * 指针要稳定,容量至少 1 字节,64 KiB 足够。 */
#define LUEYING_EXPORT_RESULT_PTR                                             \
  __attribute__((export_name("lueying_result_ptr"))) uint32_t                 \
      lueying_result_ptr(void)

#define LUEYING_EXPORT_RESULT_CAP                                             \
  __attribute__((export_name("lueying_result_cap"))) uint32_t                 \
      lueying_result_cap(void)

#ifdef __cplusplus
}
#endif

#endif /* LUEYING_EXT_H */
