#!/usr/bin/env bash
# 下载掠影词典与搭配表,钉住 lueying-dict 的 Release 标签与 sha256。
# 用法:tools/fetch_dict.sh [输出目录]
set -euo pipefail

OUT="${1:-data}"
mkdir -p "$OUT"

TAG="v0.2.0-rc1"
BASE="https://github.com/ukjent7/lueying-dict/releases/download/${TAG}"

# 下到 .part 再改名:失败时不留下一个看着齐全的半截文件。
fetch() {
  local sha="$1" name="$2"
  curl -fsSL --retry 3 -o "${OUT}/${name}.part" "${BASE}/${name}"
  echo "${sha}  ${OUT}/${name}.part" | sha256sum -c --quiet
  mv "${OUT}/${name}.part" "${OUT}/${name}"
}

fetch fef595c528ff2ac8270e713519220a4c697e7abeef4d94541d2b573b24679f6c main.pyd
fetch 0cd7a5a711d5a0f0c55ea8216064b2c19771ca3f704807ee5e9001f190983992 collocation.bin
