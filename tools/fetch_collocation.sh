#!/usr/bin/env bash
# 下载掠影原生紧凑搭配表 (LYCL v2, 约 4.8MB, 约 100 万条词对, 基于 PMI 与现代语料)。
# 发布在 lueying-dict 的 Release 上, 钉住标签与 sha256。
# 用法:tools/fetch_collocation.sh [输出目录]
set -euo pipefail

OUT="${1:-data}"
mkdir -p "$OUT"

TAG="v0.2.0-rc0"
SHA="f7a42f8a5de4d3e932c569bc292cee30994af6ab04cd2f087f93a2498ba171d6"
URL="https://github.com/ukjent7/lueying-dict/releases/download/${TAG}/collocation-v2.bin"

# 下到 .part 再改名:失败时不留下一个看着齐全的半截文件。
curl -fsSL --retry 3 -o "${OUT}/collocation.bin.part" "$URL"
echo "${SHA}  ${OUT}/collocation.bin.part" | sha256sum -c --quiet
mv "${OUT}/collocation.bin.part" "${OUT}/collocation.bin"
# 保持一份 main.gram 别名以完全兼容原有工具与测试
cp -f "${OUT}/collocation.bin" "${OUT}/main.gram"
