#!/usr/bin/env python3
"""从 V2 的开发集与测试集派生三类按真实打法设计的压力语料。

派生只看 V2 的全拼用例(clause-full / word-full),每个分半各自派生,开发集与测试集互不交叉。
同一输入永远得到同一结果(确定性哈希,不用全局随机数)。

三类:
    usermix  混打。音节的声母单独打,当且仅当它在该声母下的前两个高频字里有目标字;
             其余音节全拼。规则来自用户的打法:只有声母单打时首选就是那个字,才不全打。
    fuzzy    模糊音。随机挑 1–2 个音节换成模糊音(zh/z、ch/c、sh/s、l/n、f/h、an/ang、
             en/eng、in/ing、ian/iang、uan/uang),换后必须仍是合法音节。
    typo     手误。整串 1–2 处手误:相邻键替换、漏键、多键、相邻字母调换。

读音与字频来自 lueying-dict 的 output/main.tsv(词<TAB>频次<TAB>拼音,只取单字行)。
该文件不入库,所以派生结果 dicts/eval-v3-{dev,test}.tsv 入库,
只有换读音表时才需要重新生成。

用法:
    python3 tools/make_eval_v3.py --readings <main.tsv> \
        --dev dicts/eval-v2-dev.tsv --test dicts/eval-v2-test.tsv
"""

from __future__ import annotations

import argparse
import collections
import random
import re
import zlib
from functools import lru_cache
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PER_CATEGORY = 200
MAX_SYLLABLE = 6

# 模糊音对:(a, b) 表示 a ↔ b 双向替换。声母对只替换声母,韵母对只替换韵尾。
FUZZY_INITIAL = (("zh", "z"), ("ch", "c"), ("sh", "s"), ("l", "n"), ("f", "h"))
FUZZY_FINAL = (("an", "ang"), ("en", "eng"), ("in", "ing"), ("ian", "iang"), ("uan", "uang"))

KEY_ROWS = ("qwertyuiop", "asdfghjkl", "zxcvbnm")
SHIFT_BY_ROW = (0, 0.5, 1.0)  # 键盘交错排列的横向偏移,用于判断斜向相邻


def read_readings(path: Path) -> tuple[dict[str, str], dict[str, int], set[str], dict[str, list[str]]]:
    """返回 (字 -> 最高频读音, 字 -> 最高频, 合法音节集, 声母首字母 -> 前两个高频字)。"""
    best: dict[str, tuple[int, str]] = {}
    syllables: set[str] = set()
    per_char: dict[str, list[tuple[int, str]]] = collections.defaultdict(list)
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) < 3 or len(fields[0]) != 1 or not fields[2].isalpha():
            continue
        word, freq, reading = fields[0], int(fields[1]), fields[2]
        syllables.add(reading)
        per_char[word].append((freq, reading))
        if word not in best or freq > best[word][0]:
            best[word] = (freq, reading)
    top = {char: reading for char, (_, reading) in best.items()}
    freq_of = {char: freq for char, (freq, _) in best.items()}
    by_initial: dict[str, list[tuple[int, str]]] = collections.defaultdict(list)
    for char, options in per_char.items():
        for freq, reading in options:
            if reading[0] not in "aeo":
                by_initial[reading[0]].append((freq, char))
    first_two = {
        letter: [char for _, char in sorted(set(rows), reverse=True)[:2]]
        for letter, rows in by_initial.items()
    }
    return top, freq_of, syllables, first_two


def split_syllables(pinyin: str, chars: str, top: dict[str, str], syllables: set[str]) -> list[str] | None:
    """把连写拼音切成与字数相等的音节,优先取字的常用读音。无法切分时返回 None。"""

    @lru_cache(maxsize=None)
    def best(pos: int, index: int) -> tuple[int, tuple[str, ...]] | None:
        if index == len(chars):
            return (0, ()) if pos == len(pinyin) else None
        found = None
        for length in range(1, MAX_SYLLABLE + 1):
            part = pinyin[pos : pos + length]
            if len(part) < length:
                break
            if part not in syllables:
                continue
            rest = best(pos + length, index + 1)
            if rest is None:
                continue
            score = rest[0] + (1 if top.get(chars[index]) == part else 0)
            if found is None or score > found[0]:
                found = (score, (part,) + rest[1])
        return found

    result = best(0, 0)
    return list(result[1]) if result else None


def stable(*parts: str) -> random.Random:
    return random.Random(zlib.crc32("|".join(parts).encode("utf-8")))


def usermix(syllables: list[str], chars: str, first_two: dict[str, list[str]]) -> str:
    """只有声母单打时首选就是目标字的音节才缩写。零声母音节(a、e、o 开头)总是全打。"""
    out = []
    for syllable, char in zip(syllables, chars):
        letter = syllable[0]
        if letter in "aeo" or char not in first_two.get(letter, []):
            out.append(syllable)
        else:
            out.append(letter)
    return "".join(out)


def substitute(syllable: str, pair: tuple[str, str], legal: set[str]) -> str | None:
    first, second = pair
    for source, target in ((first, second), (second, first)):
        if pair in FUZZY_INITIAL and syllable.startswith(source):
            candidate = target + syllable[len(source) :]
        elif pair in FUZZY_FINAL and syllable.endswith(source):
            candidate = syllable[: -len(source)] + target
        else:
            continue
        if candidate in legal and candidate != syllable:
            return candidate
    return None


def fuzzy(syllables: list[str], legal: set[str], rng: random.Random) -> list[str] | None:
    options = [
        (index, candidate)
        for index, syllable in enumerate(syllables)
        for pair in FUZZY_INITIAL + FUZZY_FINAL
        if (candidate := substitute(syllable, pair, legal)) is not None
    ]
    if not options:
        return None
    count = min(len(options), rng.choice((1, 1, 2)))
    changed = list(syllables)
    for index, candidate in rng.sample(options, count):
        if changed[index] == syllables[index]:
            changed[index] = candidate
    return changed if changed != syllables else None


def neighbors(char: str) -> list[str]:
    """物理上相邻的键:同行左右一格,或相邻行横向距离不超过一个键宽。"""
    for row, keys in enumerate(KEY_ROWS):
        col = keys.find(char)
        if col >= 0:
            break
    else:
        return []
    x = col + SHIFT_BY_ROW[row]
    out = []
    for r, other in enumerate(KEY_ROWS):
        if abs(r - row) > 1:
            continue
        for c, key in enumerate(other):
            if (r, c) != (row, col) and abs(c + SHIFT_BY_ROW[r] - x) <= 1.01:
                out.append(key)
    return out


def typo(pinyin: str, rng: random.Random) -> str | None:
    """整串 1–2 处手误。返回与原串不同的结果。"""
    text = pinyin
    for _ in range(rng.choice((1, 1, 2))):
        if len(text) < 2:
            break
        at = rng.randrange(len(text))
        operation = rng.choice(("sub", "del", "ins", "swap"))
        if operation == "sub" and neighbors(text[at]):
            text = text[:at] + rng.choice(neighbors(text[at])) + text[at + 1 :]
        elif operation == "del":
            text = text[:at] + text[at + 1 :]
        elif operation == "ins" and text[at] in "".join(KEY_ROWS):
            text = text[:at] + rng.choice(neighbors(text[at]) or [text[at]]) + text[at:]
        elif operation == "swap" and at + 1 < len(text) and text[at] != text[at + 1]:
            text = text[:at] + text[at + 1] + text[at] + text[at + 2 :]
    return text if text != pinyin else None


def derive(
    rows: list[dict[str, str]],
    top: dict[str, str],
    legal: set[str],
    first_two: dict[str, list[str]],
) -> dict[str, list[str]]:
    """返回 类别 -> 派生行(制表符分隔的五列),每类最多 PER_CATEGORY 条,按哈希稳定取样。"""
    buckets: dict[str, list[tuple[float, str]]] = collections.defaultdict(list)
    for row in rows:
        category = row["category"]
        if not re.fullmatch(r"(clause|word)-full-(writing|dialog)", category):
            continue
        level, _, domain = category.split("-")
        chars = row["expected"]
        if not chars or not all("一" <= ch <= "鿿" for ch in chars):
            continue
        syllables = split_syllables(row["pinyin"], chars, top, legal)
        if syllables is None:
            continue
        key = chars + row["pinyin"]
        rng = stable("v3", category, key)
        fuzzed = fuzzy(syllables, legal, rng)
        variants = {
            "usermix": usermix(syllables, chars, first_two),
            "fuzzy": "".join(fuzzed) if fuzzed is not None else None,
            "typo": typo(row["pinyin"], rng),
        }
        for family, pinyin in variants.items():
            # 没有实际改动的派生输入等于原句,没有测量价值,跳过。
            if pinyin is None or pinyin == row["pinyin"]:
                continue
            name = f"{family}-{level}-{domain}"
            line = "\t".join((name, row["source"], pinyin, chars, row["context"]))
            buckets[name].append((stable("pick", name, key).random(), line))
    return {
        name: [line for _, line in sorted(rows_)[:PER_CATEGORY]]
        for name, rows_ in sorted(buckets.items())
    }


def read_rows(path: Path) -> list[dict[str, str]]:
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        category, source, pinyin, expected, *rest = line.split("\t")
        context = rest[0] if rest else ""
        rows.append(
            {"category": category, "source": source, "pinyin": pinyin, "expected": expected, "context": context}
        )
    return rows


HEADER = (
    "# 手误与模糊音压力语料 v3:由 V2 {half} 半派生(make_eval_v3.py,固定种子)。\n"
    "# 列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context(上文取最后 64 字)\n"
    "# category: <usermix|fuzzy|typo>-<clause|word>-<writing|dialog>\n"
    "# 派生只改输入串,期望答案取自 V2 原句。usermix 按用户打法缩写,fuzzy 换模糊音,typo 加手误。\n"
    "# 来源与许可: 同 eval-v2(gov 公共领域;kdconv、crosswoz Apache-2.0;risawoz MIT)\n"
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--readings", type=Path, required=True, help="lueying-dict 的 output/main.tsv")
    parser.add_argument("--dev", type=Path, default=ROOT / "dicts" / "eval-v2-dev.tsv")
    parser.add_argument("--test", type=Path, default=ROOT / "dicts" / "eval-v2-test.tsv")
    args = parser.parse_args()

    top, _, legal, first_two = read_readings(args.readings)
    for half, source in (("dev", args.dev), ("test", args.test)):
        derived = derive(read_rows(source), top, legal, first_two)
        lines = [line for rows in derived.values() for line in rows]
        out = ROOT / "dicts" / f"eval-v3-{half}.tsv"
        out.write_text(HEADER.format(half=half) + "\n".join(lines) + "\n", encoding="utf-8")
        counts = ", ".join(f"{name} {len(rows)}" for name, rows in derived.items())
        print(f"写出 {out.relative_to(ROOT)}: {len(lines)} 条({counts})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
