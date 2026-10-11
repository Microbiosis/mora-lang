#!/usr/bin/env python3
"""v0.104.6 D417：全量 fixture 的 `--opt=0/1/2` 三档输出快照 / 比对。

用途（两用）：
  - `--record`：把当前工作树下每个 fixture 在三档的 (exit, stdout+stderr 归一化) 写进
    `target/tmp/opt_snapshot.txt`，作为改 SSA 之前的**基线**。
  - 默认：把当前快照与磁盘上的基线逐条比对，列出任何差异。

差异判据刻意包含**stdout 与 stderr** —— 本项目最严重的一类失败是
「静默无输出、退出码 0」，只看 stdout 会把它当成「一致」。

用法：
  python scripts/opt_levels_snapshot.py --record
  python scripts/opt_levels_snapshot.py
"""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests" / "fixtures" / "e2e"
EXE = ROOT / "target" / "debug" / "mora.exe"
SNAPSHOT = ROOT / "target" / "tmp" / "opt_snapshot.txt"

# CLI 横幅 / 能力清单 —— 与 opt 档位无关，噪声。
NOISE_PREFIXES = (
    "Mora v",
    "AI:",
    "AI 原语",
    "显式 API",
    "Trait 系统",
    "Built-in",
    "v0.15 CLI",
    "⚠",
)


def normalize(text: str) -> str:
    out = []
    for line in text.replace("\r\n", "\n").split("\n"):
        line = line.strip()
        if not line:
            continue
        if line.startswith(NOISE_PREFIXES):
            continue
        out.append(line)
    return "\n".join(out)


def run(path: Path, opt: str) -> tuple[int, str]:
    proc = subprocess.run(
        [str(EXE), f"--opt={opt}", str(path)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
        encoding="utf-8",
        errors="replace",
        timeout=120,
    )
    return proc.returncode, normalize(proc.stdout + "\n" + proc.stderr)


def collect() -> str:
    lines = []
    for fixture in sorted(FIXTURES.glob("*.mora")):
        for opt in ("0", "1", "2"):
            code, text = run(fixture, opt)
            lines.append(f"=== {fixture.name} --opt={opt} exit={code}")
            lines.append(text)
    return "\n".join(lines) + "\n"


def main() -> int:
    if not EXE.exists():
        print(f"缺二进制：{EXE}（先 cargo build --bin mora）", file=sys.stderr)
        return 2

    current = collect()

    if "--record" in sys.argv:
        SNAPSHOT.parent.mkdir(parents=True, exist_ok=True)
        SNAPSHOT.write_text(current, encoding="utf-8")
        print(f"已记录基线 → {SNAPSHOT}（{len(current)} 字符）")
        return 0

    if not SNAPSHOT.exists():
        print(f"无基线可比：{SNAPSHOT}（先跑 --record）", file=sys.stderr)
        return 2

    baseline = SNAPSHOT.read_text(encoding="utf-8")

    def sections(text: str) -> dict[str, str]:
        out: dict[str, str] = {}
        key = None
        buf: list[str] = []
        for line in text.split("\n"):
            if line.startswith("=== "):
                if key is not None:
                    out[key] = "\n".join(buf).strip()
                key = line
                buf = []
            else:
                buf.append(line)
        if key is not None:
            out[key] = "\n".join(buf).strip()
        return out

    base = sections(baseline)
    now = sections(current)

    diffs = []
    for key in sorted(set(base) | set(now)):
        b, n = base.get(key, "<缺>"), now.get(key, "<缺>")
        if b != n:
            diffs.append(f"{key}\n  基线: {b!r}\n  现在: {n!r}")

    if not diffs:
        print(f"✔ {len(now)} 个 (fixture × opt) 组合与基线逐字一致")
        return 0

    print(f"✘ {len(diffs)} 个组合与基线不同：\n  - " + "\n  - ".join(diffs))
    return 1


if __name__ == "__main__":
    raise SystemExit(main())