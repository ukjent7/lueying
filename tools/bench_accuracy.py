#!/usr/bin/env python3
"""Versioned public accuracy suites; Python 3.11+, no extra packages."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import subprocess
import tempfile
import time
from collections import Counter
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

from bench_shared import ROOT, mcnemar_exact, normalize, read_result

PROTOCOL = "accuracy-v1"
BASE_REV = "d8ec6a6af3b746eaaa8dc0501a69ffd4b8b4e205"
SNAPSHOT = ROOT / "tools/bench-quick-v1.json"
DEV = ("eval-v2-dev.tsv", "eval-v3-dev.tsv")
TEST = ("eval-v2-test.tsv", "eval-v3-test.tsv")
SHARED = "eval-shared-v1.tsv"
EXPOSED = (*DEV, "eval-dev-v4.tsv", SHARED, "eval-extra-v1.tsv", "eval.txt", "eval-words.tsv")
# Order protects scarce strata from target/context de-duplication across strata.
QUOTAS = {
    "quick": {"segment": 40, "noise": 60, "mixed": 100, "daily": 20, "clean": 80},
    "nightly": {"segment": 300, "noise": 500, "mixed": 600, "daily": 80, "clean": 520},
    "release": {"segment": 300, "noise": 500, "mixed": 600, "clean": 600},
}
MANIFESTS = {
    "quick": "001c3289ac4fd62ef5894577849fd761206047fd0b0406f84be9d0746b901d11",
    "nightly": "a94ef65b63c6e11f6d22e76582f1b2654652c23c8592f8d2ecd0e14271a4dcc7",
    "release": "ed9dfaeb46ae98a5c0664c85e8edcf63bc7408d8e10f1b1af73fd2f8b3b9b4c3",
}
STAGES = {"engine": "rank", "8m": "rank_8m", "settled": "rank_settled"}
RANKS = tuple(STAGES.values()) + tuple(f"strict_{s}" for s in STAGES.values())
VALUES = (*RANKS, "picks", "pages")
QUICK_LOSS = 0.05
QUICK_ALPHA = 0.025  # Two co-primary endpoints, Bonferroni family alpha .05.
FULL_MARGIN = -0.02
FULL_ALPHA = 0.025  # Two co-primary intervals, Bonferroni family coverage >= .95.


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def digest(path: Path) -> str:
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


@dataclass(frozen=True)
class Case:
    corpus: str
    row: int
    category: str
    source: str
    keys: str
    gold: str
    context: str

    @property
    def ref(self) -> str:
        identity = (self.corpus, self.row, self.category, self.source, self.keys, self.gold, self.context)
        return sha(json.dumps(identity, ensure_ascii=False).encode())

    @property
    def intent(self) -> tuple[str, str]:
        # The legacy normalizer additionally merges 其它/其他. Use it conservatively
        # for de-duplication/overlap detection, not for scoring model ranks.
        return normalize(self.gold), self.context[-64:]

    @property
    def group(self) -> str:
        cat = self.category
        if cat == "daily":
            return "daily"
        if cat.startswith("chunk-full-"):
            return "segment"
        if cat.startswith(("chunk-fuzzy-", "chunk-typo-", "fuzzy-clause-", "typo-clause-")):
            return "noise"
        if cat.startswith(("word-initials-", "word-mixed-", "clause-mixed-")):
            return "mixed"
        if cat.startswith(("word-full-", "clause-full-")):
            return "clean"
        return ""


@lru_cache(maxsize=None)
def corpus(name: str) -> tuple[Case, ...]:
    result = []
    for text in (ROOT / "dicts" / name).read_text(encoding="utf-8").split("\n"):
        if not text or text.startswith("#"):
            continue
        fields = text.split("\t")
        if name in ("eval.txt", "eval-words.tsv") and len(fields) == 2:
            fields = ["legacy", name, *fields]
        if len(fields) not in (4, 5) or not all(fields[:4]) or "\r" in text:
            raise ValueError(f"{name}: malformed input at effective row {len(result) + 1}")
        result.append(Case(name, len(result), *fields[:4], fields[4] if len(fields) == 5 else ""))
    if not result:
        raise ValueError(f"{name}: empty corpus")
    return tuple(result)


@lru_cache(maxsize=1)
def overlap_annotations() -> dict[str, dict[str, list[str]]]:
    """Public exact overlap inventory; never reads user logs or the private archive."""
    result = {}
    for name in EXPOSED:
        rows = corpus(name)
        keyed = {(c.keys, c.intent[0]) for c in rows}
        exact = {(c.keys, c.intent) for c in rows}
        contextual = {c.intent for c in rows if c.intent[1]}
        for test_name in TEST:
            for c in corpus(test_name):
                kind = (
                    "same_key_gold_context" if (c.keys, c.intent) in exact else
                    "same_gold_context" if c.intent in contextual else
                    "same_key_gold_only" if (c.keys, c.intent[0]) in keyed else ""
                )
                if kind:
                    result.setdefault(c.ref, {}).setdefault(kind, []).append(name)
    return result


def choose(suite: str) -> tuple[list[tuple[Case, bool]], Counter]:
    names = (*DEV, SHARED) if suite != "release" else TEST
    overlap = overlap_annotations() if suite == "release" else {}
    used = set()
    picked = []
    excluded = Counter()
    for labels in overlap.values():
        if "same_key_gold_context" in labels or "same_gold_context" in labels:
            excluded["excluded_known_overlap"] += 1
        elif "same_key_gold_only" in labels:
            excluded["flagged_key_gold_only"] += 1
    for group, count in QUOTAS[suite].items():
        for domain in (("daily",) if group == "daily" else ("writing", "dialog")):
            quota = count if domain == "daily" else count // 2
            candidates = sorted(
                (c for name in names for c in corpus(name)
                 if c.group == group and (domain == "daily" or c.category.endswith(f"-{domain}"))
                 and re.fullmatch("[a-z]+", c.keys) and (suite != "quick" or len(c.keys) <= 24)),
                key=lambda c: c.ref,
            )
            added = 0
            for c in candidates:
                if c.intent in used or (overlap.get(c.ref, {}).keys() - {"same_key_gold_only"}):
                    continue
                picked.append((c, suite != "quick" or added < (8 if domain == "daily" else 2)))
                used.add(c.intent)
                added += 1
                if added == quota:
                    break
            if added != quota:
                raise ValueError(f"{suite}/{group}/{domain}: {added} of {quota} required unique targets")
    return picked, excluded


def payload(cases: list[tuple[Case, bool]]) -> str:
    return "".join(f"{c.category}\t{c.ref}\t{c.keys}\t{c.gold}\t{c.context}\t{int(settle)}\n"
                   for c, settle in cases)


def assets(root: Path) -> dict[str, str]:
    result = {}
    for script, subdir in (("fetch_dict.sh", ""), ("fetch_rerank.sh", "rerank")):
        text = (ROOT / "tools" / script).read_text(encoding="utf-8")
        pins = dict((name, checksum) for checksum, name in
                    re.findall(r"^fetch ([0-9a-f]{64}) (\S+)$", text, re.MULTILINE))
        required = ("main.pyd", "collocation.bin") if not subdir else tuple(
            f"rerank-{tier}.{ext}" for tier in ("8m", "25m") for ext in ("ort", "model.json", "vocab.json")
        )
        for name in required:
            path = Path(subdir, name)
            actual = digest(root / path)
            if pins.get(name) != actual:
                raise ValueError(f"asset missing pin or checksum mismatch: {path} ({actual})")
            result[path.as_posix()] = actual
    return result


def provenance(suite: str) -> dict[str, str]:
    names = set((*DEV, SHARED) if suite != "release" else (*TEST, *EXPOSED))
    return {name: digest(ROOT / "dicts" / name) for name in sorted(names)}


def run_arm(binary: Path, data: Path, inputs: str, cases: list[tuple[Case, bool]]) -> dict:
    start = time.monotonic()
    # Never leave gold/candidate TSVs in uploadable paths, even on a failed comparison.
    with tempfile.TemporaryDirectory(prefix="lueying-accuracy-") as tmp:
        work = Path(tmp)
        (work / "accuracy.tsv").write_text(inputs, encoding="utf-8")
        command = [
            str(binary), "--dict", str(data / "main.pyd"), "--gram", str(data / "collocation.bin"),
            "--rerank-dir", str(data / "rerank"), "--only-shared", "--accuracy", "--corpus", "accuracy.tsv",
        ]
        subprocess.run(command, check=True, cwd=work)
        out = work / "target" / "bench"
        if {p.name for p in out.iterdir()} != {"accuracy.0.tsv"}:
            raise ValueError("unexpected output set; expected exactly accuracy.0.tsv")
        meta, kind, rows = read_result(out / "accuracy.0.tsv")
        if meta.get("schema") != "lueying-bench-v2" or meta.get("corpus") != "accuracy" or kind != "items":
            raise ValueError("unexpected result schema, corpus or kind")
    expected = {c.ref: (c, settle) for c, settle in cases}
    results = {}
    for row in rows:
        ref = row["case_ref"]
        if ref in results or ref not in expected:
            raise ValueError(f"duplicate or unexpected case: {ref}")
        c, settle = expected[ref]
        if (row["category"], row["input"], row["expected"]) != (c.category, c.keys, c.gold):
            raise ValueError(f"case identity fields differ: {ref}")
        results[ref] = {field: int(row[field]) for field in VALUES}
        for field in ("ms", "ms_8m", "ms_25m"):
            ms = float(row[field])
            if not math.isfinite(ms) or ms < 0 or (field == "ms_25m" and not settle and ms != 0):
                raise ValueError(f"invalid {field}: {ref}")
    validate(results, expected)
    return {"binary": digest(binary), "seconds": time.monotonic() - start, "rows": results}


def validate(results: dict, expected: dict) -> None:
    if results.keys() != expected.keys():
        raise ValueError(f"case set differs: missing {len(expected.keys() - results.keys())}, "
                         f"extra {len(results.keys() - expected.keys())}")
    for ref, row in results.items():
        settle = expected[ref][1]
        if row.keys() != set(VALUES) or any(type(v) is not int for v in row.values()):
            raise ValueError(f"invalid result columns: {ref}")
        for field in RANKS:
            if field.endswith("settled") and not settle:
                if row[field] != -2:
                    raise ValueError(f"expected skipped 25M stage: {ref}")
            elif row[field] < -1:
                raise ValueError(f"missing result for {field}: {ref}")
        for rank in STAGES.values():
            if row[f"strict_{rank}"] >= 0 and not 0 <= row[rank] <= row[f"strict_{rank}"]:
                raise ValueError(f"strict/normalized ranks inconsistent: {ref}")
        if row["picks"] not in {-1, *range(1, 17)} or row["pages"] < 0:
            raise ValueError(f"invalid selection cost: {ref}")


@lru_cache(maxsize=None)
def log_choose(n: int) -> tuple[float, ...]:
    return tuple(math.lgamma(n + 1) - math.lgamma(k + 1) - math.lgamma(n - k + 1)
                 for k in range(n + 1))


@lru_cache(maxsize=None)
def binomial_upper(n: int, k: int, tail: float) -> float:
    """Clopper–Pearson upper limit: solve Pr_p(X <= k) = tail."""
    if k == n:
        return 1.0
    if k == 0:
        return -math.expm1(math.log(tail) / n)
    lo, hi = k / n, 1.0
    coeffs = log_choose(n)
    if k + 1 > n - k:
        indices = range(k + 1, n + 1)
        target = 1 - tail
    else:
        indices = range(k + 1)
        target = tail
    for _ in range(35):
        p = (lo + hi) / 2
        log_p, log_q = math.log(p), math.log1p(-p)
        prob = math.fsum(math.exp(coeffs[i] + i * log_p + (n - i) * log_q) for i in indices)
        beyond = prob < target if indices.start == 0 else prob > target
        if beyond:
            hi = p
        else:
            lo = p
    return hi


def paired_interval(n: int, up: int, down: int, alpha: float) -> tuple[float, float]:
    # Four one-sided marginal bounds, union bound; conservative even with zero flips.
    tail = alpha / 4
    up_lo = 1 - binomial_upper(n, n - up, tail)
    up_hi = binomial_upper(n, up, tail)
    down_lo = 1 - binomial_upper(n, n - down, tail)
    down_hi = binomial_upper(n, down, tail)
    return max(-1.0, up_lo - down_hi), min(1.0, up_hi - down_lo)


def pair(scope: list[Case], before: dict, after: dict, metric: str, k: int,
         alpha: float) -> tuple[int, int, int, int, float, float, float, float]:
    old = new = up = down = 0
    for c in scope:
        a, b = before[c.ref][metric], after[c.ref][metric]
        was, now = (a >= 0, b >= 0) if metric == "picks" else (0 <= a < k, 0 <= b < k)
        old += was
        new += now
        up += now and not was
        down += was and not now
    n = len(scope)
    lo, hi = paired_interval(n, up, down, alpha)
    return old, new, up, down, (up - down) / n, mcnemar_exact(up, down), lo, hi


def compare(suite: str, cases: list[tuple[Case, bool]], base: dict, head: dict) -> tuple[list[str], list[str]]:
    lines = [
        "### Paired accuracy",
        "",
        "| Scope | Metric | n | Base hits | Head hits | + | - | Delta pp | Paired CI pp | Exact p |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    failures = []
    decisions = []
    all_cases = [c for c, _ in cases]
    clean = [c for c in all_cases if c.group in ("daily", "clean")]
    settled = [c for c, enabled in cases if enabled]
    scopes = [(name, "all" if name != "settled" or suite != "quick" else "25M-enabled",
               all_cases if name != "settled" else settled) for name in STAGES]
    phase = "8m" if suite == "quick" else "settled"
    scopes.append((phase, "clean", clean))
    for stage, label, scope in scopes:
        for mode in ("normalized", "strict"):
            for k in (1, 5):
                metric = ("" if mode == "normalized" else "strict_") + STAGES[stage]
                primary = mode == "normalized" and stage == phase and (
                    (suite == "quick" and label == "all") or
                    (suite != "quick" and k == 1 and label in ("all", "clean"))
                )
                alpha = FULL_ALPHA if primary and suite != "quick" else 0.05
                old, new, up, down, diff, p, lo, hi = pair(scope, base, head, metric, k, alpha)
                width = f"{100 * (1 - alpha):g}%"
                lines.append(f"| {stage}/{label} | {mode} P@{k}{' *' if primary else ''} | {len(scope)} "
                             f"| {old} | {new} | {up} | {down} | {100 * diff:+.2f} "
                             f"| {width} [{100 * lo:+.2f}, {100 * hi:+.2f}] | {p:.4g} |")
                if not primary:
                    continue
                name = f"{stage}/{label} P@{k}"
                if suite == "quick":
                    if diff <= -QUICK_LOSS and p < QUICK_ALPHA:
                        failures.append(f"{name}: {diff:+.2%} with exact p={p:.4g}; regression")
                elif hi < FULL_MARGIN:
                    failures.append(f"{name}: upper bound {hi:+.2%} below -2 pp")
                elif lo > FULL_MARGIN:
                    decisions.append(f"{name}: proxy non-inferiority above -2 pp")
                else:
                    message = f"{name}: inconclusive at -2 pp"
                    (failures if suite == "release" else decisions).append(message)
    old, new, up, down, diff, p, lo, hi = pair(all_cases, base, head, "picks", 0, 0.05)
    lines.append(f"| {'8M/25M' if suite == 'quick' else 'settled'}/all | Oracle completion | {len(all_cases)} "
                 f"| {old} | {new} | {up} | {down} | {100 * diff:+.2f} "
                 f"| 95% [{100 * lo:+.2f}, {100 * hi:+.2f}] | {p:.4g} |")
    lines += ["", "Oracle-completion losses are newly stuck cases; these are not measured user costs."]
    for name, arm in (("Base", base), ("Head", head)):
        completed = [c for c in all_cases if arm[c.ref]["picks"] >= 0]
        if completed:
            keys = sum(len(c.keys) + arm[c.ref]["picks"] + arm[c.ref]["pages"] for c in completed)
            chars = sum(len(c.gold) for c in completed)
            lines.append(f"- {name}: conditional oracle keys/character {keys / chars:.3f}; "
                         f"stuck {len(all_cases) - len(completed)}/{len(all_cases)}.")
    lines += ["", "### Diagnostic cells (no gates)",
              "", "| Cell | n | Base/Head P@1 hits | P@1 delta 95% CI pp | + / - P@1 "
              "| Base/Head P@5 hits | P@5 delta 95% CI pp | + / - P@5 | Warning |",
              "|---|---:|---:|---:|---:|---:|---:|---:|---|"]
    for category in sorted({c.category for c in all_cases}):
        subset = [c for c in all_cases if c.category == category]
        cells = []
        for k in (1, 5):
            old, new, up, down, _, _, lo, hi = pair(subset, base, head, STAGES[phase], k, 0.05)
            cells.append(f"{old}/{new} | [{100 * lo:+.2f}, {100 * hi:+.2f}] | {up} / {down}")
        warning = f"small n; one case = {100 / len(subset):.2f} pp" if len(subset) < 50 else ""
        lines.append(f"| {category} | {len(subset)} | {' | '.join(cells)} | {warning} |")
    return lines + ["", *(f"- {message}" for message in decisions)], failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("suite", choices=(*QUOTAS, "capture-quick", "overlap"))
    parser.add_argument("--bin", type=Path)
    parser.add_argument("--base-bin", type=Path)
    parser.add_argument("--data", type=Path, default=ROOT / "data")
    parser.add_argument("--report", type=Path)
    parser.add_argument("--snapshot", type=Path, default=SNAPSHOT)
    args = parser.parse_args()
    if args.suite == "overlap":
        print(json.dumps(overlap_annotations(), sort_keys=True, indent=1))
        return 0
    capture = args.suite == "capture-quick"
    suite = "quick" if capture else args.suite
    if not args.bin or (suite != "quick") != bool(args.base_bin):
        parser.error("--bin is required; --base-bin is required only for nightly/release")
    t0 = time.monotonic()
    report = [f"# Accuracy: {suite} ({PROTOCOL})", ""]
    failures = []
    try:
        if capture:
            subprocess.run(
                ["git", "diff", "--quiet", BASE_REV, "--", "Cargo.toml", "Cargo.lock",
                 "rust-toolchain.toml", ".cargo", "crates", ":(exclude)crates/bench", "dicts"],
                cwd=ROOT, check=True,
            )
        cases, overlap = choose(suite)
        spec = payload(cases)
        manifest = sha(spec.encode())
        if manifest != MANIFESTS[suite]:
            raise ValueError(f"{suite} manifest changed: {manifest}")
        contract = {
            "protocol": PROTOCOL, "manifest": manifest, "assets": assets(args.data),
            "reference_revision": BASE_REV,
        }
        report += [f"- Manifest SHA-256: `{contract['manifest']}`",
                   f"- Pinned comparison revision: `{BASE_REV}`",
                   f"- Cases: {len(cases)} unique normalized target/context proxies; "
                   f"8M enabled {len(cases)}, 25M enabled {sum(settle for _, settle in cases)}."]
        if suite == "release":
            labels = overlap_annotations()
            flagged = sum("same_key_gold_only" in labels.get(c.ref, {}) for c, _ in cases)
            report += [f"- Public test pool: {sum(len(corpus(name)) for name in TEST)}; "
                       f"known exact overlaps excluded: {overlap['excluded_known_overlap']}; "
                       f"key/gold-only flags: {overlap['flagged_key_gold_only']} pool, {flagged} selected; "
                       "private acceptance not run."]
        else:
            report += ["- Dev/public regression data only; not an independent holdout."]
        report += [f"- Assets: `{json.dumps(contract['assets'], sort_keys=True)}`", ""]
        report += [f"- Input files: `{json.dumps(provenance(suite), sort_keys=True)}`", ""]
        if not capture and suite == "quick":
            snapshot = json.loads(args.snapshot.read_text(encoding="utf-8"))
            if snapshot["contract"] != contract:
                raise ValueError("quick reference differs in protocol, case manifest, assets or revision")
            base = snapshot["base"]
            validate(base["rows"], {c.ref: (c, settle) for c, settle in cases})
        if suite == "quick" and not capture:
            started = time.monotonic()
            subprocess.run([str(args.bin.resolve()), "--accuracy-smoke"], check=True)
            report += [f"- Fixed input/learning smoke passed ({time.monotonic() - started:.2f}s)."]
        head = run_arm(args.bin.resolve(), args.data.resolve(), spec, cases)
        report += [f"- Head run: {head['seconds']:.2f}s; binary SHA-256 `{head['binary']}`."]
        if capture:
            # Exclusive creation: never silently bless a changed baseline.
            with args.snapshot.open("x", encoding="utf-8") as out:
                out.write('{\n"contract": ' + json.dumps(contract, sort_keys=True)
                          + f',\n"base": {{\n"binary": "{head["binary"]}",\n"seconds": {head["seconds"]},\n'
                            '"rows": {\n')
                out.write(",\n".join(f'"{ref}": {json.dumps(row, sort_keys=True, separators=(",", ":"))}'
                                    for ref, row in sorted(head["rows"].items())))
                out.write("\n}}}\n")
        else:
            if suite != "quick":
                base = run_arm(args.base_bin.resolve(), args.data.resolve(), spec, cases)
            origin = "frozen reference capture" if suite == "quick" else "local reference arm"
            report += [f"- Base ({origin}): {base['seconds']:.2f}s; binary SHA-256 `{base['binary']}`.", ""]
            stats, failures = compare(suite, cases, base["rows"], head["rows"])
            report.extend(stats)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, SystemExit) as error:
        failures.append(f"Incomplete evaluation: {error}")
    report.extend([
        "", "* denotes a co-primary gate metric. No significance gates on diagnostic cells.",
        ("Quick gate: smoke/contract failures, or a normalized 8M P@1/P@5 loss "
         "of at least 5 pp with exact two-sided p < .025."
         if suite == "quick" else
         "Full gate: normalized settled P@1 overall and clean, with simultaneous 97.5% "
         "paired intervals per endpoint and a -2 pp margin. Crossing the margin is "
         "inconclusive (warning in nightly, stop in release)."),
        "Intervals: paired difference from Bonferroni-combined exact binomial bounds; "
        "intervals and exact McNemar p-values use an independent proxy-unit model, not independent users/sessions.",
        "Power illustration (two-sided alpha=.05, 80%, no multiplicity adjustment): "
        "at 10% discordance, 300 independent pairs detect ~5.1 pp and 2,000 ~2.0 pp; "
        "at 20%, ~7.2 pp and ~2.8 pp. This is not power for non-inferiority.",
        f"Wall time (evaluation only): {time.monotonic() - t0:.2f}s.", "",
        *(f"FAIL: {msg}" for msg in failures),
        "Result: FAIL" if failures else "Result: PASS",
    ])
    text = "\n".join(report) + "\n"
    if args.report:
        args.report.write_text(text, encoding="utf-8")
    print(text, end="")
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
