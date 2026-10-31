#!/usr/bin/env python3
"""跨引擎共享语料基准工具。

共享语料固定在 dicts/eval-shared-v1.tsv,四列:
    category<TAB>source<TAB>pinyin<TAB>expected

子命令:
    build             从 dicts/ 现有外部语料生成共享语料
    export            导出青简 / 清风 / 水杉评测器可读的输入文件
    import-qingjian   把青简 --eval-details 的 JSONL 转成统一逐条结果
    import-windinput  把清风 sentence_eval_file 的控制台表格转成统一汇总结果
    import-msime      把水杉 convert_eval --dump 的 JSONL 转成统一逐条结果
    compare           并排比较多份统一结果,输出 Markdown

统一结果 TSV 的头部用 `# key=value` 记录 schema/engine/corpus/kind:
    kind=items    category input expected rank top1 ms
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
        python3 tools/bench_shared.py compare target/bench-shared.tsv /tmp/qj-result.tsv /tmp/wind-result.tsv /tmp/msime-result.tsv
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "dicts" / "eval-shared-v1.tsv"
SCHEMA = "lueying-bench-v1"
CORPUS_NAME = "shared-v1"

# 顺序决定去重时谁留下:日常句优先,白霜脚本次之,中立难例最后。
SOURCES = (
    (ROOT / "dicts" / "eval-clean-extra.tsv", "daily", "msime+windinput"),
    (ROOT / "dicts" / "eval-frost.tsv", "frost", "rime-frost"),
    (ROOT / "dicts" / "eval-neutral.tsv", "hard", "c4-neutral"),
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
        if len(fields) != 4:
            raise SystemExit(f"{path}: 需要 4 列,实际 {len(fields)} 列: {line!r}")
        category, source, pinyin, expected = fields
        rows.append(
            {
                "category": category,
                "source": source,
                "pinyin": pinyin,
                "expected": expected,
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
        kept = 0
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line or line.startswith("#"):
                continue
            fields = line.split("\t")
            if len(fields) < 2:
                continue
            pinyin, expected = fields[0], fields[1]
            if pinyin in seen or expected in seen_text:
                duplicates += 1
                if pinyin in seen and seen[pinyin] != (category, expected):
                    conflicts += 1
                continue
            seen[pinyin] = (category, expected)
            seen_text[expected] = (category, pinyin)
            rows.append((category, source, pinyin, expected))
            kept += 1
        print(f"{path.relative_to(ROOT)}: {kept} 条 ({category})")

    header = [
        "# 跨引擎共享评测集 v1(冻结;语料或列定义变化时换 v2 文件名)",
        "# 列: category<TAB>source<TAB>pinyin<TAB>expected",
        "# category: daily=日常手写, frost=白霜脚本, hard=中立难例",
        "# 来源与许可:",
        "#   daily  metasequoiaime/msime sentences-v1 + huanfeng/WindInput grammar_sentence_eval.rs(GPL-3.0)",
        "#   frost  gaboolic/rime-frost@fbdf9ef script.lua(GPL-3.0)",
        "#   hard   metasequoiaime/msime sentences-neutral-v1,C4 中文(ODC-BY)",
        "# 判定归一化: 其它→其他、她/它→他、地/得→的",
        "# 生成: python3 tools/bench_shared.py build",
    ]
    out = "\n".join(header + ["\t".join(row) for row in rows]) + "\n"
    CORPUS.write_text(out, encoding="utf-8")
    print(
        f"写出 {CORPUS.relative_to(ROOT)}: {len(rows)} 条,"
        f"去重 {duplicates} 条,其中答案冲突 {conflicts} 条"
    )
    return 0


def cmd_export(args: argparse.Namespace) -> int:
    rows = read_corpus()
    if args.category:
        rows = [row for row in rows if row["category"] == args.category]
    if not rows:
        raise SystemExit("没有可导出的条目")
    if args.engine == "qingjian":
        # 青简冻结格式: 句子<TAB>拼音<TAB>上文
        lines = [f"{row['expected']}\t{row['pinyin']}\t" for row in rows]
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
                f"{max(syllables, 1)}\t{row['category']}\t"
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


def item_metric(rows: list[dict[str, str]]) -> dict[str, float | int | None]:
    n = len(rows)
    ranks = []
    times = []
    for row in rows:
        raw = row.get("rank", "")
        if raw not in ("", "-"):
            rank = int(raw)
            if rank >= 0:
                ranks.append(rank)
        raw_ms = row.get("ms", "")
        if raw_ms not in ("", "-"):
            times.append(float(raw_ms))
    top1 = sum(1 for rank in ranks if rank == 0)
    top3 = sum(1 for rank in ranks if rank < 3)
    top5 = sum(1 for rank in ranks if rank < 5)
    mrr = sum(1.0 / (rank + 1) for rank in ranks)
    return {
        "n": n,
        "top1": top1 * 100.0 / n if n else None,
        "top3": top3 * 100.0 / n if n else None,
        "top5": top5 * 100.0 / n if n else None,
        "mrr": mrr / n if n else None,
        "ms_avg": statistics.fmean(times) if times else None,
        "ms_p99": percentile(times, 0.99) if times else None,
    }


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(q * (len(ordered) - 1)))
    return ordered[index]


def summary_metric(row: dict[str, str]) -> dict[str, float | int | None]:
    return {
        "n": int(row.get("n", "0")),
        "top1": percent(row.get("top1", "")),
        "top3": percent(row.get("top3", "")),
        "top5": percent(row.get("top5", "")),
        "mrr": percent(row.get("mrr", "")),
        "ms_avg": percent(row.get("ms_avg", "")),
        "ms_p99": percent(row.get("ms_p99", "")),
    }


def weighted(metrics: list[dict[str, float | int | None]], key: str) -> float | None:
    pairs = [(m[key], m["n"]) for m in metrics if m.get(key) is not None and m["n"]]
    total = sum(n for _, n in pairs)
    return sum(float(value) * n for value, n in pairs) / total if total else None


def fmt(value: float | None, suffix: str = "") -> str:
    if value is None:
        return "-"
    if isinstance(value, int):
        return f"{value}{suffix}"
    return f"{value:.1f}{suffix}"


def fmt_mrr(value: float | None) -> str:
    return "-" if value is None else f"{value:.3f}"


def cmd_compare(args: argparse.Namespace) -> int:
    corpus = read_corpus()
    corpus_by_category: dict[str, int] = {}
    for row in corpus:
        corpus_by_category[row["category"]] = corpus_by_category.get(row["category"], 0) + 1

    engines: list[dict] = []
    for raw_path in args.results:
        path = Path(raw_path)
        meta, kind, rows = read_result(path)
        by_category: dict[str, list[dict[str, str]]] = {}
        for row in rows:
            by_category.setdefault(row.get("category", "unknown"), []).append(row)
        categories = {}
        for category, group in by_category.items():
            categories[category] = (
                item_metric(group) if kind == "items" else summary_metric(group[0])
            )
        if kind == "items":
            overall = item_metric(rows)
            core = item_metric([row for row in rows if row.get("category") != "hard"])
        else:
            overall = {
                key: weighted(list(categories.values()), key)
                for key in ("top1", "top3", "top5", "mrr", "ms_avg", "ms_p99")
            }
            overall["n"] = sum(int(metric["n"]) for metric in categories.values())
            core_metrics = [
                metric
                for category, metric in categories.items()
                if category != "hard"
            ]
            core = {
                key: weighted(core_metrics, key)
                for key in ("top1", "top3", "top5", "mrr", "ms_avg", "ms_p99")
            }
            core["n"] = sum(int(metric["n"]) for metric in core_metrics)
        engines.append(
            {
                "name": meta.get("engine", path.stem),
                "kind": kind,
                "categories": categories,
                "core": core,
                "overall": overall,
                "path": path,
            }
        )

    engines.sort(key=lambda engine: engine["overall"]["top1"] or -1, reverse=True)

    lines = [
        "# 共享语料基准对比",
        "",
        f"语料:`{CORPUS_NAME}`({len(corpus)} 条)。判定归一化:其它→其他、她/它→他、地/得→的。",
        "`rank` 由各引擎的候选列表按同一归一化重算;汇总档缺少的指标显示为 `-`。",
        "",
        "| 引擎 | 类别 | 条数 | top-1 | top-3 | top-5 | MRR | 平均 ms | p99 ms |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for engine in engines:
        for category in sorted(engine["categories"]):
            metric = engine["categories"][category]
            lines.append(
                f"| {engine['name']} | {category} | {metric['n']} | "
                f"{fmt(metric['top1'], '%')} | {fmt(metric['top3'], '%')} | "
                f"{fmt(metric['top5'], '%')} | {fmt_mrr(metric['mrr'])} | "
                f"{fmt(metric['ms_avg'], '')} | {fmt(metric['ms_p99'], '')} |"
            )
        metric = engine["core"]
        lines.append(
            f"| {engine['name']} | **核心** | {metric['n']} | "
            f"{fmt(metric['top1'], '%')} | {fmt(metric['top3'], '%')} | "
            f"{fmt(metric['top5'], '%')} | {fmt_mrr(metric['mrr'])} | "
            f"{fmt(metric['ms_avg'], '')} | {fmt(metric['ms_p99'], '')} |"
        )
        metric = engine["overall"]
        lines.append(
            f"| {engine['name']} | **全量** | {metric['n']} | "
            f"{fmt(metric['top1'], '%')} | {fmt(metric['top3'], '%')} | "
            f"{fmt(metric['top5'], '%')} | {fmt_mrr(metric['mrr'])} | "
            f"{fmt(metric['ms_avg'], '')} | {fmt(metric['ms_p99'], '')} |"
        )

    lines.extend(["", "## 覆盖检查", "", "| 结果 | 类型 | 缺失 | 多余 | 条数不符 |", "|---|---|---:|---:|---:|"])
    for engine in engines:
        if engine["kind"] == "items":
            got = {
                category: int(metric["n"])
                for category, metric in engine["categories"].items()
            }
            missing = sum(
                max(0, corpus_by_category.get(category, 0) - got.get(category, 0))
                for category in corpus_by_category
            )
            extra = sum(max(0, got.get(category, 0) - corpus_by_category.get(category, 0)) for category in got)
            mismatch = "-"
        else:
            missing = "-"
            extra = "-"
            mismatch = sum(
                1
                for category, count in corpus_by_category.items()
                if engine["categories"].get(category, {}).get("n") != count
            )
        lines.append(
            f"| {engine['name']} | {engine['kind']} | {missing} | {extra} | {mismatch} |"
        )

    text = "\n".join(lines) + "\n"
    if args.out:
        Path(args.out).write_text(text, encoding="utf-8")
        print(f"写出 {args.out}")
    sys.stdout.write(text)
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
        category = str(case.get("id", "")).split("-", 1)[0] or "unknown"
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

    export = sub.add_parser("export", help="导出青简/清风/水杉可读的输入文件")
    export.add_argument("--engine", choices=("qingjian", "windinput", "msime"), required=True)
    export.add_argument("--category", help="只导出某一类")
    export.add_argument("--out", required=True, help="输出路径,- 表示 stdout")
    export.set_defaults(func=cmd_export)

    compare = sub.add_parser("compare", help="并排比较多份统一结果")
    compare.add_argument("results", nargs="+", help="统一结果 TSV")
    compare.add_argument("--out", help="同时写入该路径")
    compare.set_defaults(func=cmd_compare)

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
