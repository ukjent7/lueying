#!/usr/bin/env bash
# 下载两档本地重排模型（Apache-2.0），钉住 HuggingFace 提交与 sha256。
# 数据来源:metasequoiaime/pinyin-ime-reranker-4M、pinyin-ime-reranker-25M;
# 只作为数据文件随程序分发,不修改。4M 约 4.5MB 走按键同步路径,
# 25M 约 25MB 只在停顿后异步运行。
# 用法:tools/fetch_rerank.sh [输出目录]
set -euo pipefail

OUT="${1:-data/rerank}"
mkdir -p "$OUT"

fetch() {
  local repo="$1" rev="$2" sha="$3" name="$4"
  local url="https://huggingface.co/${repo}/resolve/${rev}/sentence-model.safetensors"
  curl -fsSL --retry 3 -o "${OUT}/${name}" "$url"
  echo "${sha}  ${OUT}/${name}" | sha256sum -c -
}

fetch metasequoiaime/pinyin-ime-reranker-4M \
  e5b1f7e768d7cb2b6ff334db4e34af153920c6ff \
  86ac529510cb3b4968a5e6ade83ec8080f5b34a0a681e75c362accbbd387d1c1 \
  rerank-4m.safetensors

fetch metasequoiaime/pinyin-ime-reranker-25M \
  75295b373a7ef0d14e9bebd32c1625c9cfcfea49 \
  0a6ecba69bf1d39c7eb49549c716477dd6c03fdf05757773e1f50435262fb469 \
  rerank-25m.safetensors
