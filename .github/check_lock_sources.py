#!/usr/bin/env python3
"""Cargo.lock 来源检查:只允许 crates.io 与工作区路径,拒绝 git/未知 registry。

cargo audit 只看漏洞,这里补一条供应链来源门禁:锁文件里的每个外部包都必须
来自 crates.io。许可证检查需要 cargo-deny 的完整依赖元数据,另开任务。
"""

import sys
import tomllib
from pathlib import Path

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def main() -> int:
    lock = tomllib.loads(Path("Cargo.lock").read_text(encoding="utf-8"))
    packages = lock.get("package", [])
    bad = []
    for package in packages:
        source = package.get("source")
        if source is None or source == CRATES_IO:
            continue
        bad.append(f'{package["name"]} {package["version"]} ← {source}')
    if bad:
        print("依赖来源不在白名单:", file=sys.stderr)
        for line in bad:
            print(f"  {line}", file=sys.stderr)
        return 1
    print(f"依赖来源检查通过:{len(packages)} 个包均来自 crates.io 或工作区")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
