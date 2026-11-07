#!/usr/bin/env python3
"""解码常数的自动调参工具。

原理:这些常数是编译期 const,产品代码零运行时开销;调参用工作区补丁切换取值,
每轮一轮坐标系搜索——每个常数在其邻域各取一点,整批并行跑开发集基准,
「整句首选变好、干净全拼不退」的候选胜出,胜者带证据提交。

子命令:
  plan   输出候选矩阵 JSON(跳过与当前值相同的取值),供工作流展开成 matrix
  patch  把某个常数改成指定值(精确替换那一行)
  score  从 bench 产物算出「整句首选 + 干净全拼」两个分数,输出 JSON

基准命令与 dict-bench 相同,只换语料为开发集:
  lueying-e2e --dict data/main.pyd --gram data/collocation.bin \
    --rerank-dir data/rerank --only-shared --corpus dicts/eval-dev-v4.tsv
"""

from __future__ import annotations

import argparse
import glob
import json
import re
import sys

# (文件, 常数名, 候选取值)。当前值从源码解析,候选里跳过;名单按杠杆挑,
# 全集见 decoder.rs / channel.rs 顶部的 const 块。
PLAN = [
    ("crates/engine/src/decoder.rs", "BEAM", [18, 32]),
    ("crates/engine/src/decoder.rs", "BEAM_LAST", [64, 128]),
    ("crates/engine/src/decoder.rs", "NODE_CAP", [8, 16]),
    ("crates/engine/src/decoder.rs", "NODE_CAP_SHORT", [16, 32]),
    ("crates/engine/src/decoder.rs", "NODE_CAP_WHOLE", [64, 128]),
    ("crates/engine/src/decoder.rs", "EXACT_SENTENCE_MARGIN", [3.0, 7.0]),
    ("crates/engine/src/decoder.rs", "COMPLETION_OVERRIDE_RATIO", [8, 32]),
    ("crates/engine/src/decoder.rs", "MAX_WORD_TYPOS_LONG", [2]),
    ("crates/engine/src/channel.rs", "TYPO_SWAP", [0.3, 0.5]),
    ("crates/engine/src/channel.rs", "TYPO_NEAR_KEY", [0.5, 0.7]),
    ("crates/engine/src/channel.rs", "TYPO_MISSED", [0.7, 1.0]),
    ("crates/engine/src/channel.rs", "TYPO_EXTRA", [0.7, 1.0]),
    ("crates/engine/src/channel.rs", "TYPO_VALID_WINDOW", [1.0, 2.0]),
    ("crates/engine/src/channel.rs", "TYPO_COMPLETE_INPUT", [1.0, 2.0]),
    ("crates/engine/src/channel.rs", "ABBREV_COST", [6.0, 10.0]),
]

# 干净全拼的类别:候选不得让它们倒退,这是准入线。
CLEAN_CATEGORIES = {"clause-full-dialog", "clause-full-writing", "word-full-dialog", "word-full-writing"}

COLUMNS = ("category input expected rank top1 ms rank_8m rank_settled top1_settled "
           "pool_rank pool_len ms_8m ms_25m picks pages").split()


def const_line(path: str, name: str) -> tuple[str, str]:
    """返回常数那一行与当前值;要求恰好一处声明。"""
    text = open(path, encoding="utf-8").read()
    found = re.findall(rf"^const {name}: (?:usize|u64|f64) = ([0-9.]+);$", text, re.M)
    if len(found) != 1:
        sys.exit(f"{path}: {name} 声明 {len(found)} 处,预期 1 处")
    return found[0], text


def cmd_plan(_args: argparse.Namespace) -> None:
    candidates = []
    for path, name, values in PLAN:
        current, _ = const_line(path, name)
        for value in values:
            if str(value) == current:
                continue
            candidates.append({"name": name, "file": path, "old": current, "new": value})
    print(json.dumps(candidates))


def cmd_patch(args: argparse.Namespace) -> None:
    current, text = const_line(args.file, args.name)
    old_decl = re.search(rf"^const {args.name}: (\w+) = {re.escape(current)};$", text, re.M)
    if not old_decl:
        sys.exit(f"{args.name}: 声明行不匹配")
    line = f"const {args.name}: {old_decl.group(1)} = {args.value};"
    replaced = text[:old_decl.start()] + line + text[old_decl.end():]
    open(args.file, "w", encoding="utf-8").write(replaced)
    print(f"{args.name}: {current} -> {args.value}")


def cmd_score(args: argparse.Namespace) -> None:
    rows = []
    for path in glob.glob(f"{args.items}/eval-dev-v4*.tsv"):
        for line in open(path, encoding="utf-8"):
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            fields = line.split("\t")
            if fields[0] == "category":
                continue
            rows.append(dict(zip(COLUMNS, fields)))
    if not rows:
        sys.exit("没有评分行")
    settled = sum(1 for r in rows if r["rank_settled"] == "0")
    clean_rows = [r for r in rows if r["category"] in CLEAN_CATEGORIES]
    clean = sum(1 for r in clean_rows if r["rank_settled"] == "0")
    print(json.dumps({
        "objective": round(100 * settled / len(rows), 3),
        "clean": round(100 * clean / len(clean_rows), 3) if clean_rows else None,
        "n": len(rows),
    }, ensure_ascii=False))


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("plan")
    patch = sub.add_parser("patch")
    patch.add_argument("--name", required=True)
    patch.add_argument("--file", required=True)
    patch.add_argument("--value", required=True)
    score = sub.add_parser("score")
    score.add_argument("--items", default="target/bench")
    args = parser.parse_args()
    {"plan": cmd_plan, "patch": cmd_patch, "score": cmd_score}[args.cmd](args)


if __name__ == "__main__":
    main()
