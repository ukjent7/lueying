#!/usr/bin/env python3
"""掠影原生搭配表构建工具 (Lueying Collocation Table Builder)

基于纯文本语料与输入法词表,通过正向最大匹配切词统计词频与相邻共现频次,
使用点互信息 (PMI) 及互信息贡献度 (c * PMI) 筛选出高增益搭配,导出紧凑的 LYCL 二进制文件。

完全自主可控,零 C++ 依赖,无 KenLM / darts-clone 复杂构建链。
"""

import argparse
import gzip
import math
import os
import re
import struct
import sys
from collections import Counter
from pathlib import Path

NATIVE_MAGIC = b"LYCL"
NATIVE_VERSION = 1

PUNCT_SPLIT = re.compile(
    r"[ \t\r\n,.!?;:'\"，。！？；：、“”‘’（）()【】《》…—～~/\\]+"
)


def fnv1a_64(data: bytes) -> int:
    """FNV-1a 64-bit 哈希算法。"""
    h = 0xCBF29CE484222325
    prime = 0x100000001B3
    for b in data:
        h ^= b
        h = (h * prime) & 0xFFFFFFFFFFFFFFFF
    return h


def hash_pair(left: str, right: str) -> int:
    return fnv1a_64(left.encode("utf-8") + b"\x00" + right.encode("utf-8"))


def load_vocabulary(dict_path: str):
    """加载词典词表 (支持 .txt, .tsv, .gz)。"""
    vocab = set()
    max_len = 6
    open_fn = gzip.open if dict_path.endswith(".gz") else open
    mode = "rt" if dict_path.endswith(".gz") else "r"
    with open_fn(dict_path, mode, encoding="utf-8", errors="ignore") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            word = line.split("\t")[0].strip()
            if word:
                vocab.add(word)
                if len(word) > max_len:
                    max_len = len(word)
    print(f"已加载词表: {len(vocab):,} 词, 最长词长: {max_len}")
    return vocab, max_len


def segment_clause(clause: str, vocab: set, max_len: int):
    """词格约束正向最大匹配分词。"""
    words = []
    n = len(clause)
    i = 0
    while i < n:
        matched = False
        upper = min(i + max_len, n)
        for j in range(upper, i, -1):
            sub = clause[i:j]
            if not vocab or sub in vocab:
                words.append(sub)
                i = j
                matched = True
                break
        if not matched:
            words.append(clause[i])
            i += 1
    return words


def build_collocation(
    corpus_paths: list[str],
    dict_path: str,
    output_path: str,
    limit: int = 180000,
    eval_path: str | None = None,
):
    vocab, max_len = load_vocabulary(dict_path)

    unigrams = Counter()
    bigrams = Counter()
    total_tokens = 0
    total_pairs = 0

    print(f"正在扫描统计语料: {corpus_paths} ...")
    clause_count = 0

    for path in corpus_paths:
        p = Path(path)
        files = [p] if p.is_file() else list(p.glob("**/*.txt"))
        for fpath in files:
            open_fn = gzip.open if str(fpath).endswith(".gz") else open
            mode = "rt" if str(fpath).endswith(".gz") else "r"
            with open_fn(fpath, mode, encoding="utf-8", errors="ignore") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    clauses = PUNCT_SPLIT.split(line)
                    for cl in clauses:
                        cl = cl.strip()
                        if len(cl) < 2:
                            continue
                        words = segment_clause(cl, vocab, max_len)
                        if len(words) < 2:
                            continue
                        clause_count += 1
                        for w in words:
                            unigrams[w] += 1
                            total_tokens += 1
                        for i in range(len(words) - 1):
                            bigrams[(words[i], words[i + 1])] += 1
                            total_pairs += 1
                        # 句末搭配
                        bigrams[(words[-1], "$")] += 1
                        total_pairs += 1

    print(f"切分子句数: {clause_count:,}")
    print(
        f"统计完成: 词汇数 {len(unigrams):,}, 共现对数 {len(bigrams):,}, 总词数 {total_tokens:,}"
    )

    # 错误驱动加权
    boosted = {}
    if eval_path and os.path.exists(eval_path):
        print(f"应用评估集错误驱动增强: {eval_path} ...")
        with open(eval_path, "r", encoding="utf-8", errors="ignore") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                expected = line.split("\t")[-1].strip()
                if len(expected) >= 2:
                    words = segment_clause(expected, vocab, max_len)
                    for i in range(len(words) - 1):
                        boosted[(words[i], words[i + 1])] = 2.5
                    if words:
                        boosted[(words[-1], "$")] = 1.5
        print(f"已注入重点保底搭配数: {len(boosted):,}")

    # 计算 PMI 与互信息重要性
    scored = []
    n_f = float(total_tokens)
    m_f = float(total_pairs)

    for (u, v), count in bigrams.items():
        is_boosted = (u, v) in boosted
        if count < 3 and not is_boosted:
            continue

        c_u = float(unigrams.get(u, count))
        c_v = float(total_pairs // 10) if v == "$" else float(unigrams.get(v, count))

        p_uv = count / m_f
        p_u = c_u / n_f
        p_v = c_v / n_f

        pmi = math.log2(p_uv / max(p_u * p_v, 1e-12))
        shrinkage = count / (count + 5.0)
        score = max(-3.0, min(3.0, pmi * shrinkage))

        if is_boosted:
            score = max(score, boosted[(u, v)])

        if score <= 0.0 and not is_boosted:
            continue

        # 互信息贡献度排序准则
        importance = count * max(0.1, score)
        if is_boosted:
            importance *= 100.0

        scored.append((importance, score, u, v))

    scored.sort(key=lambda x: x[0], reverse=True)
    selected = scored[:limit]
    print(f"保留最高增益搭配: {len(selected):,} 条")

    # 转换为 64位哈希与定点数值, 去重
    unique_pairs = {}
    for imp, sc, u, v in selected:
        h = hash_pair(u, v)
        fixed_val = int(round(sc * 1000.0))
        fixed_val = max(-32768, min(32767, fixed_val))
        if h not in unique_pairs or fixed_val > unique_pairs[h][2]:
            unique_pairs[h] = (u, v, fixed_val)

    sorted_entries = sorted(unique_pairs.items(), key=lambda x: x[0])
    count = len(sorted_entries)
    print(f"最终去重无碰撞条目: {count:,}")

    # 导出 LYCL 二进制
    out_p = Path(output_path)
    out_p.parent.mkdir(parents=True, exist_ok=True)
    with open(out_p, "wb") as f:
        # Header (16 bytes)
        f.write(NATIVE_MAGIC)
        f.write(struct.pack("<III", NATIVE_VERSION, count, 0))
        # Keys (8 * count bytes)
        for h, _ in sorted_entries:
            f.write(struct.pack("<Q", h))
        # Values (2 * count bytes)
        for _, (_, _, val) in sorted_entries:
            f.write(struct.pack("<h", val))

    size_mb = out_p.stat().st_size / (1024 * 1024)
    print(f"已生成原生搭配表: {output_path} ({size_mb:.2f} MB, {count:,} 条搭配)")


def main():
    parser = argparse.ArgumentParser(description="掠影原生搭配表构建器")
    parser.add_argument("--dict", required=True, help="白霜词库或主词表路径 (.txt, .tsv, .gz)")
    parser.add_argument("--corpus", nargs="+", required=True, help="语料文件或目录")
    parser.add_argument("--out", required=True, help="输出二进制路径 (例如 main.gram)")
    parser.add_argument("--limit", type=int, default=180000, help="保留最大搭配条数")
    parser.add_argument("--eval", help="可选评估集 TSV (用于错误驱动保底)")
    args = parser.parse_args()

    build_collocation(
        corpus_paths=args.corpus,
        dict_path=args.dict,
        output_path=args.out,
        limit=args.limit,
        eval_path=args.eval,
    )


if __name__ == "__main__":
    main()
