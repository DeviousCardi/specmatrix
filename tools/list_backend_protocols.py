#!/usr/bin/env python3
"""Emit every (backend, suite) pair this repository's adapters declare, as a
JSON array, for the dogfood workflow's matrix.

Read from the adapters themselves rather than kept as a hand-maintained list:
a new adapter or a new protocol block on an existing one is picked up the next
run without anyone updating a workflow file to match.
"""
import glob
import json

import yaml


def main() -> None:
    pairs = []
    for path in sorted(glob.glob("backends/*.yaml")):
        with open(path) as handle:
            adapter = yaml.safe_load(handle)
        name = adapter["name"]
        for suite in adapter.get("protocols", {}):
            pairs.append({"backend": name, "suite": suite})
    print(json.dumps(pairs))


if __name__ == "__main__":
    main()
