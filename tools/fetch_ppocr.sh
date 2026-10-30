#!/usr/bin/env bash
# 下载 PP-OCRv6 small 的 det/rec ONNX 与词表(约 31MB)。
# 钉住 Hugging Face 仓库 revision 与每个文件的 sha256;词表从 rec 仓库的
# inference.yml 提取 character_dict(18708 行),不再单独维护一份。
# 模型许可 Apache-2.0,见 licenses/Apache-2.0.txt 与打包生成的许可说明。
# 用法:tools/fetch_ppocr.sh [输出目录](默认 data/ppocr)
set -euo pipefail

OUT="${1:-data/ppocr}"
DET_REV="28fe5895c24fd108c19eb3e8479f4ab385fbfc62"
REC_REV="b8f84f0b80c529de40b4fbb3544b84fa7233a513"
DET_SHA="d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e"
REC_SHA="5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634"
DICT_SHA="b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d"
DET_URL="https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/${DET_REV}/inference.onnx"
REC_URL="https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/${REC_REV}/inference.onnx"
REC_YML="https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/${REC_REV}/inference.yml"

mkdir -p "$OUT"

# 已有文件哈希对得上就跳过;下到 .part 再改名,失败不留半截文件。
download() {
    local url="$1" sha="$2" target="$3"
    if [ -f "$target" ] && echo "$sha  $target" | sha256sum -c --quiet 2>/dev/null; then
        return
    fi
    curl -fsSL --retry 3 -o "${target}.part" "$url"
    echo "$sha  ${target}.part" | sha256sum -c --quiet
    mv "${target}.part" "$target"
}

download "$DET_URL" "$DET_SHA" "$OUT/det.onnx"
download "$REC_URL" "$REC_SHA" "$OUT/rec.onnx"

if [ ! -f "$OUT/dict.txt" ] || ! echo "$DICT_SHA  $OUT/dict.txt" | sha256sum -c --quiet 2>/dev/null; then
    curl -fsSL --retry 3 -o "$OUT/inference.yml.part" "$REC_YML"
    # 取 character_dict 下的列表项,再按 YAML 单引号规则去引号。
    awk '
        /^[[:space:]]*character_dict:/ { in_dict = 1; next }
        in_dict && /^[[:space:]]*- / { sub(/^[[:space:]]*- /, ""); print; next }
        in_dict && !/^[[:space:]]*$/ { exit }
    ' "$OUT/inference.yml.part" \
        | sed -e "s/^'//" -e "s/'$//" \
        | sed -e "s/''/'/g" > "$OUT/dict.txt.part"
    echo "$DICT_SHA  $OUT/dict.txt.part" | sha256sum -c --quiet
    mv "$OUT/dict.txt.part" "$OUT/dict.txt"
    rm -f "$OUT/inference.yml.part"
fi

echo "PP-OCRv6 模型就绪: $OUT"
