#!/usr/bin/env bash
# 下载掠影原生紧凑搭配表 (约 1.72MB, 180,000 条词对, 基于 PMI 与现代语料)。
# 采用原生 LYCL 格式, 钉住 GitHub Release 标签与 sha256。
# 产出 collocation.bin 并软链/复制 main.gram 别名以兼顾历史脚本。
# 用法:tools/fetch_gram.sh [输出目录]
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "${DIR}/fetch_collocation.sh" "$@"
