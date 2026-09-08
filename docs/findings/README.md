# Findings, drafted for upstream

`CONTRIBUTING.md` requires every `ALTER` to be filed in the backend's own
tracker before it appears on a published page: a maintainer should learn about a
finding from their own issues, not from a comparison table.

These are the drafts. **None has been filed.** Each is written to reproduce with
`curl` alone — no Rust, no SpecMatrix — because a report a maintainer has to
reproduce for themselves before they can act on it is half a report.

When one is filed, put the issue URL in the `rule.observed` list of the check it
came from. A check that carries the bug it caught is how a reader judges whether
this project is worth their attention.

| Draft | Store | Class |
| --- | --- | --- |
| [`quickwit-int64-body-discarded.md`](quickwit-int64-body-discarded.md) | Quickwit 0.8.2 | accepted, discarded, success reported |
| [`quickwit-empty-export-500.md`](quickwit-empty-export-500.md) | Quickwit 0.8.2 | conformant traffic refused |
| [`quickwit-sort-missing-ignored.md`](quickwit-sort-missing-ignored.md) | Quickwit 0.8.2 | query returns different rows |
| [`loki-invalid-utf8-replaced.md`](loki-invalid-utf8-replaced.md) | Loki 3.1.1 | accepted, then different |
| [`victorialogs-diagnostic-as-message.md`](victorialogs-diagnostic-as-message.md) | VictoriaLogs 1.52.0 | accepted, then different |
| [`victorialogs-silent-retention-drop.md`](victorialogs-silent-retention-drop.md) | VictoriaLogs 1.52.0 | accepted, discarded, nothing reported |
| [`openobserve-response-encoding.md`](openobserve-response-encoding.md) | OpenObserve 0.92.2 | report unreadable by a conformant client |

## Tone

`CONTRIBUTING.md`: write findings the way you would want one written about your
own code. State the behaviour, cite the rule, give the reproduction, and do not
editorialise. Most divergences are a reasonable reading of an ambiguous
specification rather than carelessness. Where this project is unsure whether a
behaviour is documented, the draft asks rather than asserts.
