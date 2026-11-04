#!/usr/bin/env bash
# 下载两档本地按键重排模型（Apache-2.0），钉住 GitHub Release 标签与 sha256。
# 8M 约 8.2MB 走每键同步路径；25M 约 30.8MB 只在停顿后异步运行。
# 每个模型三个文件必须放在同一目录:rerank-8m.ort/.model.json/.vocab.json
# (25M 同名换前缀)。
# 用法:tools/fetch_rerank.sh [输出目录]
set -euo pipefail
OUT="${1:-data/rerank}"
mkdir -p "$OUT"

TAG="${RERANK_TAG:-rerank-v2}"
BASE_URL="https://github.com/ukjent7/chinese-ime-lm/releases/download/${TAG}"

fetch() {
  local sha="$1" name="$2"
  local target="${OUT}/${name}"
  if [ -f "$target" ] && echo "${sha}  ${target}" | sha256sum -c --quiet 2>/dev/null; then
    return
  fi
  local url="${BASE_URL}/${name}"
  curl -fsSL --retry 3 -o "${target}.part" "$url"
  echo "${sha}  ${target}.part" | sha256sum -c --quiet
  mv "${target}.part" "$target"
}

fetch e036febfb8f7bdf941f1637be127ff539ea95b937a4c2a27f4c6ab897138fc6c rerank-8m.ort
fetch 33e2b903a0d576730488864a007e67d518d8c7123572e6f4f77a80487fb5eda5 rerank-8m.model.json
fetch d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038 rerank-8m.vocab.json

fetch 827d9ecae4fdfb8ac06d15fbcef827705a55f72ea5e39c96f805f2f1350ee58f rerank-25m.ort
fetch fdeb4f150fd6a9f937b130ccef4f3248436a9b783202e125fd0a2d1fe3661283 rerank-25m.model.json
fetch d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038 rerank-25m.vocab.json

fetch cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 LICENSE
fetch e120d424d4d7d8cc006c7adcd02a748e83a07cdae8f35f31fefd431e56168d1e NOTICE
