#!/usr/bin/env bash
# 下载万象拼音 LTS 词级语法模型(amzxyz/rime-wanxiang-data),钉住 release 与 sha256。
# 数据来源:万象拼音 <https://github.com/amzxyz/rime-wanxiang>,许可 CC BY 4.0;
# 只作为数据文件随程序分发,不修改。模型约 380 MiB,mmap 加载,不占常驻内存。
# 用法:tools/fetch_gram.sh [输出路径]
set -euo pipefail

REV="${GRAM_REV:-LTS}"
FILE="wanxiang-lts-zh-hans.gram"
SHA="${GRAM_SHA256:-9eedac1d836f3fdafb61b162035c8937253aedc106be40b2b957d9ab64e9ed89}"
OUT="${1:-data/main.gram}"

mkdir -p "$(dirname "$OUT")"
curl -fsSL --retry 3 -o "$OUT" \
  "https://github.com/amzxyz/rime-wanxiang-data/releases/download/${REV}/${FILE}"
echo "${SHA}  ${OUT}" | sha256sum -c -
