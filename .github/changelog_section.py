#!/usr/bin/env python3
"""从 CHANGELOG.md 提取某个版本的正文，供 tag 发布写 Release 说明。

用法：
    python3 .github/changelog_section.py 0.1.0
    python3 .github/changelog_section.py 0.1.0 --out release-notes.md
    python3 .github/changelog_section.py 0.1.0 --check

标题接受 `## [0.1.0] - 2026-11-01`、`## 0.1.0` 与带 `v` 前缀的写法。
找不到版本段时返回非零，发布流程在打包前就能失败，不会发出没有说明的版本。
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHANGELOG = ROOT / "CHANGELOG.md"
HEADING = re.compile(r"^##\s+\[?v?([^\]\s]+)\]?(?:\s+-\s+.*)?$")


def section(version: str) -> str:
    version = version.removeprefix("v")
    lines = CHANGELOG.read_text(encoding="utf-8").splitlines()
    start = None
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if match and match.group(1) == version:
            start = index + 1
            break
    if start is None:
        raise LookupError(f"{CHANGELOG} 里没有版本 {version} 的二级标题")
    end = len(lines)
    for index in range(start, len(lines)):
        if lines[index].startswith("## "):
            end = index
            break
    body = "\n".join(lines[start:end]).strip()
    if not body:
        raise LookupError(f"{CHANGELOG} 的版本 {version} 没有正文")
    return body + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="版本号，可带 v 前缀")
    parser.add_argument("--out", help="写入文件；缺省打印到 stdout")
    parser.add_argument("--check", action="store_true", help="只检查版本段存在")
    args = parser.parse_args()

    try:
        body = section(args.version)
    except LookupError as error:
        print(error, file=sys.stderr)
        return 1

    if args.check:
        print(f"CHANGELOG 有版本 {args.version.removeprefix('v')} 的说明")
        return 0
    if args.out:
        Path(args.out).write_text(body, encoding="utf-8")
        print(f"写出 {args.out}")
    else:
        sys.stdout.write(body)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())