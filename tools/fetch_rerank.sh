#!/usr/bin/env bash
# 下载两档本地按键重排模型（Apache-2.0），钉住 HuggingFace 提交与 sha256。
# 8M 约 8.2MB 走每键同步路径；25M 约 30.8MB 只在停顿后异步运行。
# 每个模型三个文件必须放在同一目录:rerank-8m.ort/.model.json/.vocab.json
# (25M 同名换前缀)。
# 用法:tools/fetch_rerank.sh [输出目录]
set -euo pipefail
OUT="${1:-data/rerank}"
mkdir -p "$OUT"

fetch() {
  local repo="$1" rev="$2" sha="$3" remote="$4" name="$5"
  local url="https://huggingface.co/${repo}/resolve/${rev}/${remote}"
  curl -fsSL --retry 3 -o "${OUT}/${name}" "$url"
  echo "${sha}  ${OUT}/${name}" | sha256sum -c -
}

fetch msime/pinyin-ime-keystroke-reranker-8M \
  1ae5be9d7bb35d9e7242171a7a2868103a9e4286 \
  ceeb3a8def8ed00277063146121fe1ce152eba34006c23844a1c0c0ed188bad4 \
  lm_step.ort rerank-8m.ort
fetch msime/pinyin-ime-keystroke-reranker-8M \
  1ae5be9d7bb35d9e7242171a7a2868103a9e4286 \
  eb7ccb2fd2153906ce131891aeaa04cc1092aeb9a22eeb9543aa206443ee7ba5 \
  model.json rerank-8m.model.json
fetch msime/pinyin-ime-keystroke-reranker-8M \
  1ae5be9d7bb35d9e7242171a7a2868103a9e4286 \
  d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038 \
  vocab.json rerank-8m.vocab.json

fetch msime/pinyin-ime-keystroke-reranker-25M \
  c492a22f47445ed53501d214a2de671d87959218 \
  ad729f94fc10cae0326269e2f089d7747ee8efae3dd2affe365c2740c59a4a93 \
  lm_step.ort rerank-25m.ort
fetch msime/pinyin-ime-keystroke-reranker-25M \
  c492a22f47445ed53501d214a2de671d87959218 \
  2a92e6aad513335e0800ff24ef7037acb365d0ae9abb7ae5b92ebf241c065a20 \
  model.json rerank-25m.model.json
fetch msime/pinyin-ime-keystroke-reranker-25M \
  c492a22f47445ed53501d214a2de671d87959218 \
  d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038 \
  vocab.json rerank-25m.vocab.json

fetch msime/pinyin-ime-keystroke-reranker-8M \
  1ae5be9d7bb35d9e7242171a7a2868103a9e4286 \
  cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 \
  LICENSE LICENSE
fetch msime/pinyin-ime-keystroke-reranker-8M \
  1ae5be9d7bb35d9e7242171a7a2868103a9e4286 \
  b64ea435176bc4147d2d76aeded37460e82487b4dd8d8e5d97bae6809a7bbabe \
  NOTICE NOTICE
