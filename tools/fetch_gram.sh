#!/usr/bin/env bash
# 下载精简语法模型(约 5.3MB),钉住 WindInput 提交与 sha256。
# 上游是万象拼音 LTS 语法模型(CC-BY-4.0),WindInput 按真实输入语料裁剪成子集;
# 我们只作为数据文件随程序分发,不再修改。许可与署名见 licenses/CC-BY-4.0.txt
# 和打包时生成的 licenses/README.md。
# 用法:tools/fetch_gram.sh [输出目录]
set -euo pipefail

OUT="${1:-data}"
mkdir -p "$OUT"

REV="a99a7ad35701ff87dac6bd2057b0f594cd004d81"
SHA="c85801930ab5703d28472f86153f37a367287156ef70a523c5a4fb07f6a95147"
URL="https://raw.githubusercontent.com/huanfeng/WindInput/${REV}/data/schemas/pinyin/grammar/wanxiang-mini.gram"

# 下到 .part 再改名:失败时不留下一个看着齐全的半截文件。
curl -fsSL --retry 3 -o "${OUT}/main.gram.part" "$URL"
echo "${SHA}  ${OUT}/main.gram.part" | sha256sum -c --quiet
mv "${OUT}/main.gram.part" "${OUT}/main.gram"
