#!/usr/bin/env bash
# 下载 rime 词级语言模型(lotem/rime-octagram-data),钉住 release 与 sha256。
# 数据来源:LGPL-3.0;只作为数据文件随程序分发,不修改。
# 用法:tools/fetch_gram.sh [输出路径]
set -euo pipefail

REV="${GRAM_REV:-20260712}"
FILE="zh-hans-t-essay-bgw-compact.gram"
SHA="${GRAM_SHA256:-d3cb2438c1fdcd6a855dd6ca8f5c1060a29273c6b64c2c2c69af67cd71b6aa7e}"
OUT="${1:-data/main.gram}"

mkdir -p "$(dirname "$OUT")"
curl -fsSL --retry 3 -o "$OUT" \
  "https://github.com/lotem/rime-octagram-data/releases/download/${REV}/${FILE}"
echo "${SHA}  ${OUT}" | sha256sum -c -
