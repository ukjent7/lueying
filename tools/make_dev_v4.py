#!/usr/bin/env python3
"""由 V2 开发集与 V3 分段语料派生调参用的开发集 v4。

原则:调参只看与真实打法相关、且有区分力的样本;整句只留少量做回归。

    word-full / word-initials / word-mixed  词级,每类每领域最多 300 条(词级便宜,但要防重复)
    chunk-full / chunk-fuzzy / chunk-typo   分段 1–3 字,带真实上文,来自 eval-v3-dev.tsv(每类 200 条)
    clause-full / clause-mixed              整句回归,每领域 150 个目标句,只留全拼与混打两种输入
    去掉 clause-initials(整句声母不是核心)与 fuzzy-clause/typo-clause(与整句回归重复)

选取全部由哈希稳定决定,不用随机数;只读 eval-v2-dev 与 eval-v3-dev,不触碰测试集。

用法:
    python3 tools/make_dev_v4.py
"""

from __future__ import annotations

import collections
import re
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORD_PER_CELL = 300
CLAUSE_SENTENCES_PER_DOMAIN = 150
WORD_CATEGORIES = ("word-full", "word-initials", "word-mixed")
CLAUSE_CATEGORIES = ("clause-full", "clause-mixed")
CHUNK_CATEGORIES = ("chunk-full", "chunk-fuzzy", "chunk-typo")
DOMAINS = ("writing", "dialog")
HEADER = (
    "# 调参开发集 v4:由 V2 开发集(词级、整句回归)与 V3 分段语料派生(make_dev_v4.py,固定哈希选取)。\n"
    "# 列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context\n"
    "# 去掉整句声母与整句手误/模糊音(与整句回归重复)。测试集仍为 eval-v2-test / eval-v3-test,不用于调参。\n"
)


def read(path: Path) -> list[dict[str, str]]:
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        category, source, pinyin, expected, *rest = line.split("\t")
        out.append(
            {
                "category": category,
                "source": source,
                "pinyin": pinyin,
                "expected": expected,
                "context": rest[0] if rest else "",
            }
        )
    return out


def rank(*parts: str) -> int:
    return zlib.crc32("|".join(parts).encode("utf-8"))


def main() -> int:
    v2 = read(ROOT / "dicts" / "eval-v2-dev.tsv")
    v3 = read(ROOT / "dicts" / "eval-v3-dev.tsv")
    kept: list[tuple[str, str]] = []  # (排序键, 输出行)

    def emit(row: dict[str, str]) -> None:
        line = "\t".join((row["category"], row["source"], row["pinyin"], row["expected"], row["context"]))
        kept.append((line, line))

    # 词级:每 (类别, 领域) 按哈希取前 300 条,去掉同一 (目标, 输入) 的重复。
    cells: dict[tuple[str, str], list[dict[str, str]]] = collections.defaultdict(list)
    for row in v2:
        level_style, _, domain = row["category"].rpartition("-")
        if level_style in WORD_CATEGORIES and domain in DOMAINS:
            cells[(level_style, domain)].append(row)
    for (level_style, domain), rows in sorted(cells.items()):
        seen: set[tuple[str, str]] = set()
        unique = []
        for row in sorted(rows, key=lambda r: rank(r["category"], r["expected"], r["pinyin"])):
            key = (row["expected"], row["pinyin"])
            if key not in seen:
                seen.add(key)
                unique.append(row)
        for row in unique[:WORD_PER_CELL]:
            emit(row)

    # 整句回归:每领域按哈希取 150 个目标句;全拼与混打共用同一批句子。
    sentences: dict[str, set[str]] = collections.defaultdict(set)
    for row in v2:
        category = row["category"]
        style = category.split("-")[1]
        domain = category.split("-")[2]
        if category.startswith("clause-") and style in ("full", "mixed") and domain in DOMAINS:
            sentences[domain].add(row["expected"])
    chosen = {
        domain: set(sorted(texts, key=lambda t: rank("clause", domain, t))[:CLAUSE_SENTENCES_PER_DOMAIN])
        for domain, texts in sentences.items()
    }
    for row in v2:
        category = row["category"]
        if not category.startswith("clause-"):
            continue
        style, domain = category.split("-")[1], category.split("-")[2]
        if style in ("full", "mixed") and row["expected"] in chosen.get(domain, set()):
            emit(row)

    # 分段:v3 中的 chunk-* 整体保留(每类已在派生时取到 200 条)。
    for row in v3:
        if row["category"].startswith(tuple(f"{c}-" for c in CHUNK_CATEGORIES)):
            emit(row)

    kept.sort(key=lambda item: item[0])
    out = ROOT / "dicts" / "eval-dev-v4.tsv"
    out.write_text(HEADER + "\n".join(line for _, line in kept) + "\n", encoding="utf-8")
    counts = collections.Counter(line.split("\t")[0] for _, line in kept)
    print(f"写出 {out.relative_to(ROOT)}: {len(kept)} 条")
    for name, count in sorted(counts.items()):
        print(f"  {name:24s}{count:6d}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
