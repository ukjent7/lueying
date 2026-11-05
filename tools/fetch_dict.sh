#!/usr/bin/env bash
# 下载掠影词典与搭配表,钉住 lueying-dict 的 Release 标签与 sha256。
# 用法:tools/fetch_dict.sh [输出目录]
set -euo pipefail

OUT="${1:-data}"
mkdir -p "$OUT"

TAG="v0.2.0"
BASE="https://github.com/ukjent7/lueying-dict/releases/download/${TAG}"

# 下到 .part 再改名:失败时不留下一个看着齐全的半截文件。
fetch() {
  local sha="$1" name="$2"
  curl -fsSL --retry 5 --retry-delay 15 -o "${OUT}/${name}.part" "${BASE}/${name}"
  echo "${sha}  ${OUT}/${name}.part" | sha256sum -c --quiet
  mv "${OUT}/${name}.part" "${OUT}/${name}"
}

fetch 6f43573d267667823142375b6952e25b4ec38e6b20f2af2f3b171866074c1340 main.pyd
fetch 5644dac72b696d250fbe58cd19f78caa0035f5412dee6286a5b737468c1d75d2 collocation.bin
