#!/usr/bin/env bash
# 下载白霜拼音词库(gaboolic/rime-frost)的 cn_dicts,钉住 commit 保证可重复。
set -euo pipefail

# shellcheck disable=SC2016
REV="${FROST_REV:-fbdf9efc4a5036db5f37b97323e1b936e8fe993c}"
OUT="${1:-data/frost}"
BASE="https://raw.githubusercontent.com/gaboolic/rime-frost/${REV}/cn_dicts"

mkdir -p "$OUT"
for f in base 8105 41448 ext tencent; do
  curl -fsSL --retry 3 -o "$OUT/$f.dict.yaml" "$BASE/$f.dict.yaml"
  echo "$f: $(grep -c $'\t' "$OUT/$f.dict.yaml") 行"
done
