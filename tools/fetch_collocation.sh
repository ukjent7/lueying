#!/usr/bin/env bash
# 下载掠影原生紧凑搭配表 (约 1.72MB, 180,000 条词对, 基于 PMI 与现代语料)。
# 采用原生 LYCL 格式, 钉住 GitHub Release 标签与 sha256。
# 用法:tools/fetch_collocation.sh [输出目录]
set -euo pipefail

OUT="${1:-data}"
mkdir -p "$OUT"

TAG="v0.1.0"
SHA="6bd18dbb19c6353ad41701dc410c54bc9a2fa1566d1cbc499cbfaa0d663a591f"
URL="https://github.com/ukjent7/lueying-gram/releases/download/${TAG}/collocation.bin"

# 下到 .part 再改名:失败时不留下一个看着齐全的半截文件。
curl -fsSL --retry 3 -o "${OUT}/collocation.bin.part" "$URL"
echo "${SHA}  ${OUT}/collocation.bin.part" | sha256sum -c --quiet
mv "${OUT}/collocation.bin.part" "${OUT}/collocation.bin"
# 保持一份 main.gram 别名以完全兼容原有工具与测试
cp -f "${OUT}/collocation.bin" "${OUT}/main.gram"
