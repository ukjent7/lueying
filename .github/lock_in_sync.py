#!/usr/bin/env python3
"""检查 Cargo.lock 是否还跟得上清单。

清单加了依赖而锁没跟上时,CI 里所有 `--locked` 的命令都会红,而 fmt job
只在「这次 push 动了清单」时才重新解析——上一次回推失败,锁就永远补不上。
这个脚本离线比对清单里的直接依赖与锁里的记录,给 fmt job 一个不依赖
git diff、也不拉远端依赖的判据。

退出码 0 = 同步,1 = 失配(需要重新解析依赖)。
"""

from __future__ import annotations

import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent


def locked_dependencies() -> dict[str, set[str]]:
    """锁里每个包记录的直接依赖名。"""
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    out: dict[str, set[str]] = {}
    for package in lock.get("package", []):
        names = set()
        for entry in package.get("dependencies", []):
            # 条目形如 "serde 1.0.210 (registry+...)" 或 "lueying-render"。
            names.add(entry.split(" ")[0])
        out[package["name"]] = names
    return out


def manifest_dependencies(manifest: pathlib.Path) -> set[str]:
    """清单里声明的直接依赖名(含 target、dev、build 三类)。"""
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    names: set[str] = set()
    tables = [data]
    tables.extend(data.get("target", {}).values())
    for table in tables:
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            for name, spec in table.get(key, {}).items():
                # 改名依赖在锁里记的是包名,不是清单里的键。
                if isinstance(spec, dict) and "package" in spec:
                    names.add(spec["package"])
                else:
                    names.add(name)
    return names


def main() -> int:
    locked = locked_dependencies()
    missing: list[str] = []
    for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
        recorded = locked.get(package["name"], set())
        for name in sorted(manifest_dependencies(manifest)):
            if name not in recorded:
                missing.append(f"{package['name']} -> {name}")
    if missing:
        print("锁与清单失配:" + ", ".join(missing))
        return 1
    print("锁与清单同步")
    return 0


if __name__ == "__main__":
    sys.exit(main())
