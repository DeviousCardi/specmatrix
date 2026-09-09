#!/usr/bin/env python3
"""Report how long since each adapter was last confirmed by hand.

`AGENTS.md` requires every adapter to open with a comment naming the image
and the date it was confirmed against a running container — this reads that
comment rather than adding a second, machine-only field that could drift from
it. A backend whose adapter cannot be re-confirmed in a quarter must still be
shown, with its last date and a note, never silently carried forward as if it
were current — this is what lets the quarterly workflow write that note
instead of pretending nothing happened.

Exit code is always 0: a stale adapter is not a build failure, it is
something a maintainer needs to see. That is what "never silently carried
forward" excludes — not running it, not showing it.
"""
import argparse
import datetime
import glob
import re
import sys

# The convention every adapter opens with, e.g. "Confirmed by hand against
# grafana/tempo:3.0.3 on 2026-09-09." A second "Confirmed by hand ... on"
# further down the file documents one field, not the whole adapter, so only
# the first match — always in the header comment — counts.
PATTERN = re.compile(r"Confirmed by hand.*?on (\d{4}-\d{2}-\d{2})")

# A quarter plus a little slack for the maintainer window the plan describes,
# rather than the calendar quarter itself, which would flag an adapter
# confirmed three days into a new quarter as already stale.
STALE_AFTER_DAYS = 100


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--as-of", help="ISO date to measure staleness from (default: today)")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    as_of = (
        datetime.date.fromisoformat(args.as_of) if args.as_of else datetime.date.today()
    )

    rows = []
    for path in sorted(glob.glob("backends/*.yaml")):
        with open(path) as handle:
            text = handle.read()
        match = PATTERN.search(text)
        name = path.split("/")[-1].removesuffix(".yaml")
        if not match:
            rows.append({"backend": name, "confirmed": None, "age_days": None, "stale": True})
            continue
        confirmed = datetime.date.fromisoformat(match.group(1))
        age = (as_of - confirmed).days
        rows.append({
            "backend": name,
            "confirmed": confirmed.isoformat(),
            "age_days": age,
            "stale": age > STALE_AFTER_DAYS,
        })

    if args.json:
        import json
        print(json.dumps(rows, indent=2))
        return 0

    stale = [r for r in rows if r["stale"]]
    for row in rows:
        marker = "STALE" if row["stale"] else "ok"
        confirmed = row["confirmed"] or "no confirmation comment found"
        age = f"{row['age_days']}d" if row["age_days"] is not None else "?"
        print(f"{row['backend']:20s} {marker:5s} confirmed {confirmed} ({age})")
    if stale:
        names = ", ".join(r["backend"] for r in stale)
        print(
            f"\n{len(stale)} adapter(s) not re-confirmed within {STALE_AFTER_DAYS} days: {names}",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
