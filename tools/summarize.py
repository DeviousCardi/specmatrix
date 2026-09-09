#!/usr/bin/env python3
"""Render one `specmatrix run --json` outcome as a job-summary table.

Used by `action.yml`. Every row printed here came from the wire — the
verdict was decided the same way whether or not the case id appears in the
adapter's `allow:` list. What `allow:` changes is only where the row is
printed: reviewed rows move to their own section so a maintainer's dashboard
does not read every accepted, understood divergence as an unreviewed one.
"""
import argparse
import json
import sys


def table(rows: list[dict]) -> str:
    if not rows:
        return "_none_\n"
    lines = ["| Check | Verdict | Detail |", "| --- | --- | --- |"]
    for row in rows:
        detail = row["detail"].replace("|", "\\|").replace("\n", " ")
        lines.append(f"| `{row['id']}` | {row['verdict'].upper()} | {detail} |")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("matrix_json")
    parser.add_argument("--line-only", action="store_true")
    args = parser.parse_args()

    with open(args.matrix_json) as handle:
        outcome = json.load(handle)

    results = outcome["results"]
    counts: dict[str, int] = {}
    for row in results:
        counts[row["verdict"]] = counts.get(row["verdict"], 0) + 1
    total = len(results)
    line = (
        f"{total} checks, {counts.get('pass', 0)} pass, {counts.get('reject', 0)} reject, "
        f"{counts.get('alter', 0)} alter, {counts.get('n/a', 0)} n/a"
    )

    if args.line_only:
        print(line)
        return 0

    reviewed = [r for r in results if r.get("allowed_reason")]
    unreviewed = [r for r in results if not r.get("allowed_reason")]

    version = outcome.get("backend_version") or "version unknown"
    print(f"## specmatrix: {outcome['backend']} {version} — {outcome['suite']}\n")
    print(f"{line}\n")
    print(table(unreviewed))
    if reviewed:
        print("\n### Allowed — read and accepted by this adapter's maintainer\n")
        for row in reviewed:
            reason = row["allowed_reason"]
            print(f"- `{row['id']}` ({row['verdict'].upper()}): {reason}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
