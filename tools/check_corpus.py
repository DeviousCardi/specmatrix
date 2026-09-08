#!/usr/bin/env python3
"""Every check must cite a rule with a basis.

docs/TEST-CASES.md: checks invented to pad the matrix make it look thorough and
make it worthless. If a check cannot cite why it exists, it does not go in. That
is a rule about people, so it is enforced by a machine.
"""
import glob
import sys

import yaml

BASES = ("spec", "de-facto")


def main() -> int:
    problems = []
    cases = sorted(glob.glob("cases/*/*.yaml"))
    for path in cases:
        with open(path) as handle:
            case = yaml.safe_load(handle)
        rule = case.get("rule") or {}
        if rule.get("basis") not in BASES:
            problems.append(f"{path}: rule.basis must be one of {BASES}")
        if not (rule.get("text") or "").strip():
            problems.append(f"{path}: rule.text is empty — quote the clause or the bug")
        if not case.get("id") or not case.get("protocol"):
            problems.append(f"{path}: needs an id and a protocol")
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    print(f"{len(cases)} cases, every one citing a rule with a basis")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
