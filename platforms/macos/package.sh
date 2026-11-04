#!/usr/bin/env bash
# 构建 macOS 输入法 .app 包。产物在 dist/Lueying.app。
# 数据文件可选:存在 main.pyd/main.gram/rerank-* 就一起打包,
# 缺失时输入法退回内置词库并关闭重排。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="${PROFILE:-release}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
APP="$ROOT/dist/Lueying.app"

cd "$ROOT"
cargo build --profile "$PROFILE" -p lueying-macos --locked

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/data"
cp "$TARGET_DIR/$PROFILE/lueying-macos" "$APP/Contents/MacOS/"
cp "$ROOT/platforms/macos/ime/Info.plist" "$APP/Contents/Info.plist"

copy_data() {
  local source="$1"
  local name="$2"
  if [ -f "$source" ]; then
    cp "$source" "$APP/Contents/Resources/data/$name"
  fi
}

copy_data "$ROOT/data/main.pyd" main.pyd
copy_data "$ROOT/data/main.tsv" main.tsv
copy_data "$ROOT/data/collocation.bin" collocation.bin
copy_data "$ROOT/data/main.gram" main.gram
for name in \
  rerank-8m.ort rerank-8m.model.json rerank-8m.vocab.json \
  rerank-25m.ort rerank-25m.model.json rerank-25m.vocab.json; do
  copy_data "$ROOT/data/rerank/$name" "$name"
  copy_data "$ROOT/data/$name" "$name"
done

mkdir -p "$APP/Contents/Resources/licenses"
for name in LICENSE NOTICE; do
  if [ -f "$ROOT/data/rerank/$name" ]; then
    cp "$ROOT/data/rerank/$name" "$APP/Contents/Resources/licenses/$name"
  fi
done

# 本地 ad-hoc 签名,方便直接拖进 ~/Library/Input Methods 试用。
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true

echo "已生成 $APP"
