#!/usr/bin/env python3
"""Lint bench scenarios for grader leakage.

A scenario's system prompt is the harness the agent runs under. When it
restates the conditions of its own `assert.sql`, the bench measures prompt
following, not capability. Harness2 refuses prompt text containing grader
words outright; here the result is a report, because for a product agent a
rule like "exactly one audit row" may be intentional. Decide per scenario.

Usage: python3 bench/lint_scenarios.py [scenario ...]
Exit 0 always; findings go to stdout.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent / "scenarios"

GRADER_WORDS = ("benchmark", "evaluation", "grader", "rubric", "score", "assert")

# Words too common to count as evidence of leakage on their own.
STOP = set(
    "the a an and or of to in on for with is are be by as at from that this "
    "all any each every not no null true false select insert update where "
    "into values set row rows table column columns must exactly one per it its "
    "than more less equal into then if when only when".split()
)


def phrases(text: str, n: int = 4) -> set[tuple[str, ...]]:
    """Lowercased content-word n-grams, so a restated condition matches even
    when the surrounding wording differs."""
    words = [w for w in re.findall(r"[a-z_][a-z0-9_.]*", text.lower()) if w not in STOP]
    return {tuple(words[i : i + n]) for i in range(max(0, len(words) - n + 1))}


def assert_comment_lines(sql: str) -> str:
    """The human-readable part of assert.sql: its comments."""
    return "\n".join(line.lstrip("- ").strip() for line in sql.splitlines() if line.strip().startswith("--"))


def lint(scenario: pathlib.Path) -> list[str]:
    out: list[str] = []
    prompt_p = scenario / "system_prompt.txt"
    assert_p = scenario / "assert.sql"
    if not prompt_p.is_file():
        return out
    prompt = prompt_p.read_text(encoding="utf-8")
    low = prompt.lower()
    for w in GRADER_WORDS:
        if re.search(rf"\b{w}\w*", low):
            out.append(f"grader word {w!r} in system_prompt.txt")
    if assert_p.is_file():
        comments = assert_comment_lines(assert_p.read_text(encoding="utf-8"))
        shared = phrases(prompt) & phrases(comments)
        if shared:
            sample = ", ".join(" ".join(p) for p in sorted(shared)[:4])
            out.append(
                f"{len(shared)} condition phrase(s) from assert.sql comments restated in the prompt "
                f"(e.g. {sample})"
            )
    always_on = len(prompt)
    if always_on > 6000:
        out.append(f"always-on prompt is {always_on} chars; consider moving procedure into a skill")
    return out


def main(argv: list[str]) -> int:
    names = argv or sorted(p.name for p in ROOT.iterdir() if p.is_dir())
    total = 0
    for name in names:
        findings = lint(ROOT / name)
        if not findings:
            print(f"{name}: clean")
            continue
        total += len(findings)
        print(f"{name}:")
        for f in findings:
            print(f"  - {f}")
    print(f"\n{total} finding(s) across {len(names)} scenario(s). Report only; nothing was changed.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
