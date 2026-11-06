#!/usr/bin/env python3
"""跨引擎共享语料基准工具。

共享语料固定在 dicts/eval-shared-v1.tsv,四列:
    category<TAB>source<TAB>pinyin<TAB>expected

子命令:
    build             从 dicts/ 现有外部语料生成共享语料
    build-v2          从 sentences-ctx-v1 生成 V2 开发集与测试集
    export            导出青简 / 清风 / 水杉评测器可读的输入文件
    import-qingjian   把青简 --eval-details 的 JSONL 转成统一逐条结果
    import-windinput  把清风 sentence_eval_file 的控制台表格转成统一汇总结果
    import-msime      把水杉 convert_eval --dump 的 JSONL 转成统一逐条结果
    summarize         把逐条结果(可含分片)合成 summary.json,含门限判定
    diff              逐条对比两份结果:增量、McNemar p 与被改坏的样本,JSON 输出

统一结果 TSV 的头部用 `# key=value` 记录 schema/engine/corpus/kind:
    kind=items    category input expected rank top1 ms [rank_8m rank_settled top1_settled pool_rank pool_len ms_8m ms_25m]
    kind=summary  category n top1 top3 top5 mrr ms_avg ms_p99

rank 是期望答案在候选里的 0 基下标,-1 表示不在候选里。百分比一律用 0-100。
判定归一化与 E2E 一致:其它→其他、她/它→他、地/得→的。

跨引擎跑法(参考项目都在 work/ 下,不参与本仓库 CI):
    青简:
        python3 tools/bench_shared.py export --engine qingjian --out /tmp/qj.tsv
        (cd work/qingjian-main && cargo run --release -p qingjian-cli -- --eval-text /tmp/qj.tsv --eval-details /tmp/qj.jsonl)
        python3 tools/bench_shared.py import-qingjian --details /tmp/qj.jsonl --out /tmp/qj-result.tsv
    清风:
        python3 tools/bench_shared.py export --engine windinput --out /tmp/wind.tsv
        (cd work/WindInput-main/wind_input && WIND_SENTENCE_EVAL=/tmp/wind.tsv cargo test --release -p wind-engine --test grammar_sentence_eval sentence_eval_file -- --ignored --nocapture > /tmp/wind.txt)
        python3 tools/bench_shared.py import-windinput --stdout /tmp/wind.txt --out /tmp/wind-result.tsv
    水杉:
        python3 tools/bench_shared.py export --engine msime --out /tmp/msime.tsv
        (cd work/msime-develop && cargo run --release -p msime-input-runtime --example convert_eval -- \
          --resources <已校验资源目录> --set /tmp/msime.tsv --dump /tmp/msime.jsonl)
        python3 tools/bench_shared.py import-msime --dump /tmp/msime.jsonl --out /tmp/msime-result.tsv
    对比:
        python3 tools/bench_shared.py diff target/bench /tmp/msime-result.tsv --samples 5
"""

from __future__ import annotations

import argparse
import gzip
import json
import math
import statistics
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "dicts" / "eval-shared-v1.tsv"
SCHEMA = "lueying-bench-v1"
CORPUS_NAME = "shared-v1"
V2_CONTEXT = 64

# 顺序决定去重时谁留下:日常句优先,白霜脚本次之,中立难例最后。
SOURCES = (
    (ROOT / "dicts" / "eval-clean-extra.tsv", "daily", "msime+windinput"),
    (ROOT / "dicts" / "eval-frost.tsv", "frost", "rime-frost"),
    (ROOT / "dicts" / "eval-neutral.tsv", "hard", "c4-neutral"),
    (ROOT / "dicts" / "eval-context.tsv", "context", "sentences-ctx-v1"),
)

NORMALIZE = str.maketrans({"她": "他", "它": "他", "地": "的", "得": "的"})


def normalize(text: str) -> str:
    return text.replace("其它", "其他").translate(NORMALIZE)


def read_corpus(path: Path = CORPUS) -> list[dict[str, str]]:
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) < 4:
            raise SystemExit(f"{path}: 需要至少 4 列,实际 {len(fields)} 列: {line!r}")
        category, source, pinyin, expected = fields[:4]
        context = fields[4] if len(fields) > 4 else ""
        rows.append(
            {
                "category": category,
                "source": source,
                "pinyin": pinyin,
                "expected": expected,
                "context": context,
            }
        )
    return rows


def cmd_build(args: argparse.Namespace) -> int:
    seen: dict[str, tuple[str, str]] = {}
    seen_text: dict[str, tuple[str, str]] = {}
    rows = []
    duplicates = 0
    conflicts = 0
    for path, category, source in SOURCES:
        if not path.exists():
            continue
        kept = 0
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line or line.startswith("#"):
                continue
            fields = line.split("\t")
            if len(fields) < 2:
                continue
            pinyin, expected = fields[0], fields[1]
            context = fields[2] if len(fields) > 2 else ""
            if pinyin in seen or expected in seen_text:
                duplicates += 1
                if pinyin in seen and seen[pinyin] != (category, expected):
                    conflicts += 1
                continue
            seen[pinyin] = (category, expected)
            seen_text[expected] = (category, pinyin)
            rows.append((category, source, pinyin, expected, context))
            kept += 1
        print(f"{path.relative_to(ROOT)}: {kept} 条 ({category})")

    header = [
        "# 跨引擎共享评测集 v1(冻结;语料或列定义变化时换 v2 文件名)",
        "# 列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context",
        "# category: daily=日常手写, frost=白霜脚本, hard=中立难例, context=真实上下文整句",
        "# 来源与许可:",
        "#   daily    metasequoiaime/msime sentences-v1 + huanfeng/WindInput grammar_sentence_eval.rs(GPL-3.0)",
        "#   frost    gaboolic/rime-frost@fbdf9ef script.lua(GPL-3.0)",
        "#   hard     metasequoiaime/msime sentences-neutral-v1,C4 中文(ODC-BY)",
        "#   context  metasequoiaime/chinese-ime-lm sentences-ctx-v1(gov:公共领域;dialog:Apache-2.0/MIT)",
        "# 判定归一化: 其它→其他、她/它→他、地/得→的",
        "# 生成: python3 tools/bench_shared.py build",
    ]
    lines = header + [
        f"{cat}\t{src}\t{pin}\t{exp}" + (f"\t{ctx}" if ctx else "")
        for cat, src, pin, exp, ctx in rows
    ]
    out = "\n".join(lines) + "\n"
    CORPUS.write_text(out, encoding="utf-8")
    print(
        f"写出 {CORPUS.relative_to(ROOT)}: {len(rows)} 条,"
        f"去重 {duplicates} 条,其中答案冲突 {conflicts} 条"
    )
    return 0


def cmd_build_v2(args: argparse.Namespace) -> int:
    """从 sentences-ctx-v1 全量生成 V2 的开发集与测试集。

    按 crc32(unit) 分半,同一篇文档或同一段对话不会跨两边。偶数半是词典学 EM 权重用的
    开发集,调参只看它;奇数半是测试集,只在发版时跑。
    """
    with gzip.open(args.source, "rt", encoding="utf-8") as f:
        cases = [json.loads(line) for line in f]
    halves: dict[str, list[str]] = {"dev": [], "test": []}
    for case in cases:
        domain = "writing" if case["source"] == "gov" else "dialog"
        category = f"{case['level']}-{case['style']}-{domain}"
        context = case["context"].replace("\t", " ").replace("\n", " ")[-V2_CONTEXT:]
        half = "dev" if zlib.crc32(case["unit"].encode()) % 2 == 0 else "test"
        halves[half].append(f"{category}\t{case['source']}\t{case['input']}\t{case['gold']}\t{context}")
    head = (
        "# 共享语料 V2:chinese-ime-lm eval/sentences-ctx-v1 全量,{half} 半。\n"
        "# 列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context(上文取最后 {size} 字)\n"
        "# category: <clause|word>-<full|initials|mixed>-<writing|dialog>\n"
        "# 来源与许可: gov 公共领域;kdconv、crosswoz Apache-2.0;risawoz MIT\n"
    )
    for half, lines in halves.items():
        path = ROOT / "dicts" / f"eval-v2-{half}.tsv"
        path.write_text(head.format(half=half, size=V2_CONTEXT) + "\n".join(lines) + "\n", encoding="utf-8")
        print(f"写出 {path.relative_to(ROOT)}: {len(lines)} 条")
    return 0


def cmd_export(args: argparse.Namespace) -> int:
    rows = read_corpus(args.corpus)
    if args.category:
        rows = [row for row in rows if row["category"] == args.category]
    if getattr(args, "strip_context", False):
        for row in rows:
            row["context"] = ""
    if not rows:
        raise SystemExit("没有可导出的条目")
    if args.engine == "qingjian":
        # 青简冻结格式: 句子<TAB>拼音<TAB>上文
        lines = [f"{row['expected']}\t{row['pinyin']}\t{row['context']}" for row in rows]
    elif args.engine == "windinput":
        # 清风 sentence_eval_file: 类别<TAB>拼音<TAB>期望
        lines = [f"{row['category']}\t{row['pinyin']}\t{row['expected']}" for row in rows]
    elif args.engine == "msime":
        # 水杉 convert_eval: id<TAB>拼音<TAB>期望<TAB>音节数<TAB>标签<TAB>上下文
        # 全键盘评测不读音节数;九键评测应另用其自带九键集。
        lines = []
        for index, row in enumerate(rows):
            syllables = sum(1 for ch in row["expected"] if "\u4e00" <= ch <= "\u9fff")
            lines.append(
                f"{row['category']}-{index}\t{row['pinyin']}\t{row['expected']}\t"
                f"{max(syllables, 1)}\t{row['category']}\t{row['context']}"
            )
    else:
        raise SystemExit(f"未知引擎 {args.engine}")
    text = "\n".join(lines) + "\n"
    if args.out == "-":
        sys.stdout.write(text)
    else:
        Path(args.out).write_text(text, encoding="utf-8")
        print(f"写出 {args.out}: {len(lines)} 条")
    return 0


def read_result(path: Path) -> tuple[dict[str, str], str, list[dict[str, str]]]:
    meta: dict[str, str] = {}
    header: list[str] | None = None
    rows: list[dict[str, str]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line:
            continue
        if line.startswith("#"):
            body = line[1:].strip()
            if "=" in body:
                key, value = body.split("=", 1)
                meta[key.strip()] = value.strip()
            continue
        fields = line.split("\t")
        if header is None:
            header = fields
            continue
        rows.append(dict(zip(header, fields)))
    if header is None:
        raise SystemExit(f"{path}: 缺少表头")
    kind = meta.get("kind") or ("items" if "rank" in header else "summary")
    return meta, kind, rows


def percent(value: str) -> float | None:
    value = value.strip()
    if not value or value == "-":
        return None
    return float(value.rstrip("%"))


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(q * (len(ordered) - 1)))
    return ordered[index]


def wilson_interval(successes: int, total: int) -> tuple[float, float]:
    """top-1 比例的 Wilson 95% 区间,以百分数返回。"""
    if total == 0:
        return (0.0, 0.0)
    p = successes / total
    z = 1.959963984540054
    denominator = 1.0 + z * z / total
    center = (p + z * z / (2.0 * total)) / denominator
    half = (
        z
        * math.sqrt(p * (1.0 - p) / total + z * z / (4.0 * total * total))
        / denominator
    )
    return (max(0.0, center - half) * 100.0, min(1.0, center + half) * 100.0)


def mcnemar_exact(rescue: int, breakage: int) -> float:
    """两名选手不一致对的 McNemar 精确检验(双侧)。rescue 只在一方对,breakage 只在另一方对。"""
    discordant = rescue + breakage
    if discordant == 0:
        return 1.0
    tail = sum(math.comb(discordant, i) for i in range(min(rescue, breakage) + 1))
    return min(1.0, 2.0 * tail / (2**discordant))


STAGES = {"engine": "rank", "8m": "rank_8m", "settled": "rank_settled"}
POOLS = (9, 32, 64, 128)
# 每类的最低线:只防整条链路系统性退步,按引擎层算,数值与全量基准的外部下限一致。
GATES = {"daily": (85.0, 90.0), "frost": (65.0, 80.0), "hard": (50.0, 75.0), "context": (60.0, 80.0)}
# 核心场景(按产品真正面对的打法):全拼、词级简拼与混打、整句混打、分段(chunk)与模糊音、手误压力集。
# 纯声母整句不计入:不是核心打法,只记录。
CORE_PREFIXES = ("clause-full-", "word-full-", "word-initials-", "word-mixed-", "clause-mixed-",
                 "chunk-", "fuzzy-", "typo-")
EXCLUDED_FROM_CORE = ("clause-initials-",)
# 干净全拼用例:打得准的部分,首选准确率不能低于基线(见 --baseline)。
CLEAN_PREFIXES = ("clause-full-", "word-full-", "chunk-full-")


def read_items(paths: list[Path]) -> tuple[dict[str, str], list[dict[str, str]]]:
    """读一份或多份逐条结果,分片在这里合并。"""
    meta: dict[str, str] = {}
    rows: list[dict[str, str]] = []
    for path in paths:
        part_meta, kind, part = read_result(path)
        if kind != "items":
            raise SystemExit(f"{path}: 需要 kind=items 的逐条结果")
        meta.update(part_meta)
        rows.extend(part)
    return meta, rows


def rank_in(row: dict[str, str], stage: str) -> int:
    """没有这一层的结果(其他引擎)时退回 rank。"""
    raw = row.get(STAGES[stage]) or row.get("rank") or "-1"
    return int(raw)


def rates(rows: list[dict[str, str]], stage: str) -> dict[str, float | int]:
    ranks = [rank_in(row, stage) for row in rows]
    hit = [rank for rank in ranks if rank >= 0]
    n = len(rows)
    low, high = wilson_interval(sum(rank == 0 for rank in hit), n)
    return {
        "n": n,
        "top1": round(sum(rank == 0 for rank in hit) * 100.0 / n, 2),
        "top1_ci": [round(low, 2), round(high, 2)],
        "top3": round(sum(rank < 3 for rank in hit) * 100.0 / n, 2),
        "top5": round(sum(rank < 5 for rank in hit) * 100.0 / n, 2),
        "mrr": round(sum(1.0 / (rank + 1) for rank in hit) / n, 4),
        "missing": round((n - len(hit)) * 100.0 / n, 2),
    }


def timing(rows: list[dict[str, str]], column: str) -> dict[str, float] | None:
    values = [float(row[column]) for row in rows if float(row.get(column) or 0) > 0]
    if not values:
        return None
    return {
        "avg": round(statistics.fmean(values), 2),
        "p95": round(percentile(values, 0.95), 2),
        "p99": round(percentile(values, 0.99), 2),
    }


def cost(rows: list[dict[str, str]]) -> dict[str, float] | None:
    """打出一句话的成本:选词与翻页次数,以及根本打不出来的比例。"""
    picks = [int(row["picks"]) for row in rows if row.get("picks")]
    if not picks:
        return None
    done = [value for value in picks if value > 0]
    pages = [int(row["pages"]) for row in rows if row.get("pages")]
    return {
        "picks": round(statistics.fmean(done), 3) if done else 0.0,
        "pages": round(statistics.fmean(pages), 3),
        "stuck": round((len(picks) - len(done)) * 100.0 / len(picks), 2),
    }


def is_core(category: str) -> bool:
    return category.startswith(CORE_PREFIXES) and not category.startswith(EXCLUDED_FROM_CORE)


def summarize_rows(rows: list[dict[str, str]]) -> dict[str, object]:
    categories = sorted({row["category"] for row in rows})
    by_category = {
        category: {
            **{stage: rates([row for row in rows if row["category"] == category], stage) for stage in STAGES},
            "cost": cost([row for row in rows if row["category"] == category]),
        }
        for category in categories
    }
    out: dict[str, object] = {
        "all": {stage: rates(rows, stage) for stage in STAGES},
        # 宏平均:每类同权,不让条数最多的一类决定总分。
        "macro": {
            stage: {
                key: round(statistics.fmean(c[stage][key] for c in by_category.values()), 2)
                for key in ("top1", "top5")
            }
            for stage in STAGES
        },
        "categories": by_category,
        "ms": {column: timing(rows, column) for column in ("ms", "ms_8m", "ms_25m")},
    }
    core = [category for category in categories if is_core(category)]
    if core:
        # 核心宏平均:每个核心类别同权,不让条数多的一类决定总分。
        out["core"] = {
            "categories": core,
            **{
                stage: {
                    key: round(statistics.fmean(by_category[c][stage][key] for c in core), 2)
                    for key in ("top1", "top5")
                }
                for stage in STAGES
            },
        }
    pool = [int(row["pool_rank"]) for row in rows if row.get("pool_rank")]
    if pool:
        out["pool_hit"] = {
            str(size): round(sum(0 <= rank < size for rank in pool) * 100.0 / len(pool), 2)
            for size in POOLS
        }
    return out


def gate_failures(summary: dict[str, object]) -> list[str]:
    failed = []
    categories = summary["categories"]
    for category, (top1, top5) in GATES.items():
        metric = categories.get(category, {}).get("engine") if categories else None
        if metric and (metric["top1"] < top1 or metric["top5"] < top5):
            failed.append(f"{category}: top1 {metric['top1']} < {top1} 或 top5 {metric['top5']} < {top5}")
    return failed


def clean_top1(section: dict[str, object]) -> tuple[float, int]:
    """干净全拼类别在产品链路(settled)上的加权首选准确率与条数。"""
    categories = section["categories"]
    picked = [c for name, c in categories.items() if name.startswith(CLEAN_PREFIXES)]
    total = sum(c["settled"]["n"] for c in picked)
    hit = sum(c["settled"]["top1"] * c["settled"]["n"] for c in picked)
    return (hit / total if total else 0.0), total


def baseline_failures(section: dict[str, object], base: dict[str, object], tolerance: float) -> list[str]:
    """打得准的用例首选准确率不能比基线低超过 tolerance(百分点)。"""
    now, n_now = clean_top1(section)
    before, n_before = clean_top1(base)
    if n_now == 0 or n_before == 0:
        return []
    if now < before - tolerance:
        return [f"干净全拼首选 {now:.2f} < 基线 {before:.2f} - {tolerance}(n={n_now})"]
    return []


def cmd_summarize(args: argparse.Namespace) -> int:
    """把一个目录下的分片合成 summary.json;每份语料一节。"""
    baseline = json.loads(Path(args.baseline).read_text(encoding="utf-8")) if args.baseline else None
    groups: dict[str, list[Path]] = {}
    for path in sorted(Path(args.dir).glob("*.tsv")):
        groups.setdefault(path.name.split(".")[0], []).append(path)
    if not groups:
        raise SystemExit(f"{args.dir}: 没有结果")
    summary: dict[str, object] = {}
    failed: list[str] = []
    for corpus, paths in groups.items():
        meta, rows = read_items(paths)
        section = summarize_rows(rows)
        section["engine_name"] = meta.get("engine", "")
        section["gate_failures"] = gate_failures(section)
        if baseline and corpus in baseline:
            section["baseline_failures"] = baseline_failures(section, baseline[corpus], args.tolerance)
            section["gate_failures"] += section["baseline_failures"]
        failed += section["gate_failures"]
        summary[corpus] = section
    text = json.dumps(summary, ensure_ascii=False, indent=1)
    Path(args.dir, "summary.json").write_text(text, encoding="utf-8")
    print(text if args.print else f"写出 {args.dir}/summary.json")
    return 1 if args.gate and failed else 0


def cmd_diff(args: argparse.Namespace) -> int:
    """逐条对比两份结果:每类的增量、McNemar p 与翻转样本,JSON 输出。"""
    def load(spec: str) -> tuple[dict[tuple[str, str], dict[str, str]], str]:
        path, _, stage = spec.partition(":")
        target = Path(path)
        paths = sorted(target.glob("*.tsv")) if target.is_dir() else [target]
        rows = read_items(paths)[1]
        return {(row["category"], row["input"]): row for row in rows}, stage or "settled"

    base, base_stage = load(args.base)
    cand, cand_stage = load(args.candidate)
    keys = [key for key in base if key in cand]
    out: dict[str, object] = {"common": len(keys)}
    for category in sorted({key[0] for key in keys}) + ["all"]:
        scope = [key for key in keys if category in ("all", key[0])]
        entry: dict[str, object] = {"n": len(scope)}
        for top in (1, 5):
            def hit(row: dict[str, str], stage: str) -> bool:
                return 0 <= rank_in(row, stage) < top

            rescued = [key for key in scope if hit(cand[key], cand_stage) and not hit(base[key], base_stage)]
            broken = [key for key in scope if hit(base[key], base_stage) and not hit(cand[key], cand_stage)]
            entry[f"top{top}"] = {
                "delta": round((len(rescued) - len(broken)) * 100.0 / len(scope), 2),
                "rescued": len(rescued),
                "broken": len(broken),
                "p": round(mcnemar_exact(len(rescued), len(broken)), 5),
            }
            if top == 1 and category != "all" and args.samples:
                entry["broken_samples"] = [
                    {
                        "input": key[1],
                        "expected": base[key].get("expected", ""),
                        "got": cand[key].get("top1_settled") or cand[key].get("top1", ""),
                    }
                    for key in broken[: args.samples]
                ]
        out[category] = entry
    print(json.dumps(out, ensure_ascii=False, indent=1))
    return 0


def cmd_import_qingjian(args: argparse.Namespace) -> int:
    corpus = read_corpus()
    details: dict[str, dict] = {}
    for line in Path(args.details).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get("pinyin"):
            details[row["pinyin"]] = row

    out_rows = []
    for case in corpus:
        row = details.get(case["pinyin"])
        if row is None:
            continue
        candidates = [normalize(text) for text in row.get("candidates", [])]
        want = normalize(case["expected"])
        if candidates:
            rank = candidates.index(want) if want in candidates else -1
        else:
            position = row.get("position")
            rank = int(position) if position is not None else -1
        top1 = row.get("top", "")
        out_rows.append(
            (
                case["category"],
                case["pinyin"],
                case["expected"],
                str(rank),
                top1,
                f"{float(row.get('query_ms') or 0.0):.3f}",
            )
        )

    lines = [
        f"# schema={SCHEMA}",
        f"# engine={args.engine_version}",
        f"# corpus={CORPUS_NAME}",
        "# kind=items",
        "category\tinput\texpected\trank\ttop1\tms",
    ]
    lines.extend("\t".join(row) for row in out_rows)
    text = "\n".join(lines) + "\n"
    Path(args.out).write_text(text, encoding="utf-8")
    print(f"写出 {args.out}: {len(out_rows)}/{len(corpus)} 条")
    return 0


def cmd_import_windinput(args: argparse.Namespace) -> int:
    rows = []
    header_seen = False
    for line in Path(args.stdout).read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) < 9:
            continue
        if fields[0] == "类别":
            header_seen = True
            continue
        if not header_seen:
            continue
        category, config, n, top1, _loose, top3, _chars, avg_ms, p99_ms = fields[:9]
        if args.config and args.config not in config:
            continue
        rows.append((category, n, top1, top3, avg_ms, p99_ms))

    lines = [
        f"# schema={SCHEMA}",
        f"# engine={args.engine_version}",
        f"# corpus={CORPUS_NAME}",
        "# kind=summary",
        "category\tn\ttop1\ttop3\ttop5\tmrr\tms_avg\tms_p99",
    ]
    lines.extend(
        f"{category}\t{n}\t{top1}\t{top3}\t-\t-\t{avg_ms}\t{p99_ms}"
        for category, n, top1, top3, avg_ms, p99_ms in rows
    )
    text = "\n".join(lines) + "\n"
    Path(args.out).write_text(text, encoding="utf-8")
    print(f"写出 {args.out}: {len(rows)} 个类别")
    return 0


def cmd_import_msime(args: argparse.Namespace) -> int:
    rows = []
    for line in Path(args.dump).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        case = json.loads(line)
        candidates = []
        for candidate in case.get("candidates", []):
            text = candidate.get("text") if isinstance(candidate, dict) else candidate
            if text is not None:
                candidates.append(str(text))
        expected = str(case.get("gold", ""))
        want = normalize(expected)
        normalized = [normalize(text) for text in candidates]
        rank = normalized.index(want) if want in normalized else -1
        top1 = candidates[0] if candidates else ""
        category = str(case.get("id", "")).rsplit("-", 1)[0] or "unknown"
        rows.append((category, str(case.get("input", "")), expected, str(rank), top1, "0.000"))

    lines = [
        f"# schema={SCHEMA}",
        f"# engine={args.engine_version}",
        f"# corpus={CORPUS_NAME}",
        "# kind=items",
        "category\tinput\texpected\trank\ttop1\tms",
    ]
    lines.extend("\t".join(row) for row in rows)
    text = "\n".join(lines) + "\n"
    Path(args.out).write_text(text, encoding="utf-8")
    print(f"写出 {args.out}: {len(rows)} 条")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    build = sub.add_parser("build", help="从现有外部语料生成共享语料")
    build.set_defaults(func=cmd_build)

    build_v2 = sub.add_parser("build-v2", help="从 sentences-ctx-v1 生成 V2 开发集与测试集")
    build_v2.add_argument("--source", required=True, help="sampled.jsonl.gz")
    build_v2.set_defaults(func=cmd_build_v2)

    export = sub.add_parser("export", help="导出青简/清风/水杉可读的输入文件")
    export.add_argument("--engine", choices=("qingjian", "windinput", "msime"), required=True)
    export.add_argument("--category", help="只导出某一类")
    export.add_argument("--corpus", type=Path, default=CORPUS, help="默认冻结的 shared-v1")
    export.add_argument("--strip-context", action="store_true", help="强制剥离上文以作对照")
    export.add_argument("--out", required=True, help="输出路径,- 表示 stdout")
    export.set_defaults(func=cmd_export)

    summarize = sub.add_parser("summarize", help="目录下的逐条结果(可含分片)→ summary.json")
    summarize.add_argument("dir")
    summarize.add_argument("--gate", action="store_true", help="门限不达标时退出码非零")
    summarize.add_argument("--print", action="store_true", help="同时打印到标准输出")
    summarize.add_argument("--baseline", help="上一次的 summary.json;干净全拼首选准确率不能低于它")
    summarize.add_argument("--tolerance", type=float, default=0.0, help="允许下降的百分点,默认 0")
    summarize.set_defaults(func=cmd_summarize)

    diff = sub.add_parser("diff", help="逐条对比两份结果,JSON 输出;路径后可跟 :engine/:8m/:settled")
    diff.add_argument("base")
    diff.add_argument("candidate")
    diff.add_argument("--samples", type=int, default=0, help="每类列多少条被改坏的样本")
    diff.set_defaults(func=cmd_diff)

    qingjian = sub.add_parser("import-qingjian", help="青简 --eval-details JSONL → 统一逐条结果")
    qingjian.add_argument("--details", required=True)
    qingjian.add_argument("--out", required=True)
    qingjian.add_argument("--engine-version", default="qingjian")
    qingjian.set_defaults(func=cmd_import_qingjian)

    windinput = sub.add_parser("import-windinput", help="清风 sentence_eval_file 输出 → 统一汇总结果")
    windinput.add_argument("--stdout", required=True)
    windinput.add_argument("--out", required=True)
    windinput.add_argument("--engine-version", default="windinput")
    windinput.add_argument("--config", default="基线", help="只取配置名包含该子串的行")
    windinput.set_defaults(func=cmd_import_windinput)

    msime = sub.add_parser("import-msime", help="水杉 convert_eval --dump JSONL → 统一逐条结果")
    msime.add_argument("--dump", required=True, help="convert_eval --dump 写出的逐条候选 JSONL")
    msime.add_argument("--out", required=True)
    msime.add_argument("--engine-version", default="msime")
    msime.set_defaults(func=cmd_import_msime)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
