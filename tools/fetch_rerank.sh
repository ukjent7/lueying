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
  ea91fcbc94ab8d801faab250ee02d78ed0de5b77 \
  e036febfb8f7bdf941f1637be127ff539ea95b937a4c2a27f4c6ab897138fc6c \
  lm_step.ort rerank-8m.ort
fetch msime/pinyin-ime-keystroke-reranker-8M \
  ea91fcbc94ab8d801faab250ee02d78ed0de5b77 \
  33e2b903a0d576730488864a007e67d518d8c7123572e6f4f77a80487fb5eda5 \
  model.json rerank-8m.model.json
fetch msime/pinyin-ime-keystroke-reranker-8M \
  ea91fcbc94ab8d801faab250ee02d78ed0de5b77 \
  d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038 \
  vocab.json rerank-8m.vocab.json

fetch msime/pinyin-ime-keystroke-reranker-25M \
  9ce963999bd40ebede9f73f7e39a200e8d50f25e \
  827d9ecae4fdfb8ac06d15fbcef827705a55f72ea5e39c96f805f2f1350ee58f \
  lm_step.ort rerank-25m.ort
fetch msime/pinyin-ime-keystroke-reranker-25M \
  9ce963999bd40ebede9f73f7e39a200e8d50f25e \
  fdeb4f150fd6a9f937b130ccef4f3248436a9b783202e125fd0a2d1fe3661283 \
  model.json rerank-25m.model.json
fetch msime/pinyin-ime-keystroke-reranker-25M \
  9ce963999bd40ebede9f73f7e39a200e8d50f25e \
  d7fb2e70de9f0c2c65b822a8fbf604655bfef6d0dcd8f1975d73f1cd5ea7f038 \
  vocab.json rerank-25m.vocab.json

fetch msime/pinyin-ime-keystroke-reranker-8M \
  ea91fcbc94ab8d801faab250ee02d78ed0de5b77 \
  cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 \
  LICENSE LICENSE
fetch msime/pinyin-ime-keystroke-reranker-8M \
  ea91fcbc94ab8d801faab250ee02d78ed0de5b77 \
  e120d424d4d7d8cc006c7adcd02a748e83a07cdae8f35f31fefd431e56168d1e \
  NOTICE NOTICE
