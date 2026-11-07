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

# (文件, 常数名, 类型, 步长)。候选=当前值±步长,沿上次采纳的位置继续爬;
# 步长按杠杆定,浮点格式化到与原声明一致的位数。GitHub matrix 会把 3.0
# 渲染成 3,候选一律以字符串输出,patch 再按声明类型兜底。
PLAN = [
    ("crates/engine/src/decoder.rs", "BEAM", "int", 8),
    ("crates/engine/src/decoder.rs", "BEAM_LAST", "int", 32),
    ("crates/engine/src/decoder.rs", "NODE_CAP", "int", 4),
    ("crates/engine/src/decoder.rs", "NODE_CAP_SHORT", "int", 8),
    ("crates/engine/src/decoder.rs", "NODE_CAP_WHOLE", "int", 32),
    ("crates/engine/src/decoder.rs", "EXACT_SENTENCE_MARGIN", "float2", 2.0),
    ("crates/engine/src/decoder.rs", "COMPLETION_OVERRIDE_RATIO", "int", 8),
    ("crates/engine/src/decoder.rs", "MAX_WORD_TYPOS_LONG", "int", 1),
    ("crates/engine/src/channel.rs", "TYPO_SWAP", "float2", 0.1),
    ("crates/engine/src/channel.rs", "TYPO_NEAR_KEY", "float2", 0.1),
    ("crates/engine/src/channel.rs", "TYPO_MISSED", "float2", 0.15),
    ("crates/engine/src/channel.rs", "TYPO_EXTRA", "float2", 0.15),
    ("crates/engine/src/channel.rs", "TYPO_VALID_WINDOW", "float2", 0.5),
    ("crates/engine/src/channel.rs", "TYPO_COMPLETE_INPUT", "float2", 0.5),
    ("crates/engine/src/channel.rs", "ABBREV_COST", "float2", 2.0),
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


def fmt(kind: str, value: float) -> str:
    if kind == "int":
        return str(int(value))
    return f"{value:.2f}"


def cmd_plan(_args: argparse.Namespace) -> None:
    candidates = []
    for path, name, kind, step in PLAN:
        current, _ = const_line(path, name)
        base = float(current)
        for delta in (-step, step):
            value = base + delta
            if kind == "int":
                value = max(1, value)
            text = fmt(kind, value)
            if text == current or value <= 0:
                continue
            candidates.append({"name": name, "file": path, "old": current, "new": text})
    print(json.dumps(candidates))


def cmd_patch(args: argparse.Namespace) -> None:
    current, text = const_line(args.file, args.name)
    old_decl = re.search(rf"^const {args.name}: (\w+) = {re.escape(current)};$", text, re.M)
    if not old_decl:
        sys.exit(f"{args.name}: 声明行不匹配")
    value = args.value
    # 类型兜底:f64 缺小数点补上,整数混进小数点剥掉——matrix 传参会剥 .0。
    if old_decl.group(1) == "f64" and "." not in value and "e" not in value:
        value += ".0"
    if old_decl.group(1) != "f64" and "." in value:
        value = value.split(".")[0] or value
    line = f"const {args.name}: {old_decl.group(1)} = {value};"
    replaced = text[:old_decl.start()] + line + text[old_decl.end():]
    open(args.file, "w", encoding="utf-8").write(replaced)
    print(f"{args.name}: {current} -> {value}")


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
