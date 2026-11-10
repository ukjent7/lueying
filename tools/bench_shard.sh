#!/usr/bin/env bash
# 共享语料的一片:bench_shard.sh <lueying-bench> <工作目录> <分片序号>/<总片数>
# 逐条结果写到 <工作目录>/target/bench/。私有语料与用户语料(eval-user-*)的结果只留 GnuPG 加密文件,
# 需要 EVAL_KEY,没有它时只跑公开语料。TEST=true 时加跑 V2、V3 测试集(只在发版时)。
set -euo pipefail
bin=$(realpath "$1")
cd "$2"
shard=$3
root=$(cd "$(dirname "$0")/.." && pwd)
args=(--dict "$root/data/main.pyd" --gram "$root/data/collocation.bin" --rerank-dir "$root/data/rerank" --only-shared --shard "$shard")
run() {
  if [ -n "$1" ]; then "$bin" "${args[@]}" --corpus "$1"; else "$bin" "${args[@]}"; fi
}
corpora=("$root/dicts/eval-extra-v1.tsv" "$root/dicts/eval-dev-v4.tsv")
if [ -n "${EVAL_KEY:-}" ]; then
  corpora+=("$root"/dicts/eval-user-v{1,2,3,4}.tsv)
  private=$(mktemp -d)
  trap 'rm -rf "$private"' EXIT
  printf %s "$EVAL_KEY" | gpg --batch --quiet --yes --pinentry-mode loopback --passphrase-fd 0 \
    --decrypt "$root/dicts/eval-private.tar.gpg" | tar xz -C "$private"
  corpora+=("$private/eval-private-dev.tsv")
  if [ "${TEST:-}" = true ]; then corpora+=("$private/eval-private-test.tsv"); fi
fi
if [ "${TEST:-}" = true ]; then corpora+=("$root/dicts/eval-v2-test.tsv" "$root/dicts/eval-v3-test.tsv"); fi
run ""
for corpus in "${corpora[@]}"; do run "$corpus"; done
for file in target/bench/eval-private-*.tsv target/bench/eval-user-*.tsv; do
  [ -f "$file" ] || continue
  printf %s "$EVAL_KEY" | gpg --batch --quiet --yes --pinentry-mode loopback --passphrase-fd 0 \
    --symmetric --cipher-algo AES256 -o "$file.gpg" "$file"
  rm "$file"
done
