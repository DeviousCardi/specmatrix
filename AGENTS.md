# Working on SpecMatrix

For contributors, human or agent. It covers the design you need in order to
change anything safely, and how to make each kind of change. Planning documents
are not in this repository.

## What the tool does

Sends payloads that are unusual but valid to an observability backend, reads
them back, and reports what the backend did with them. A check is declarative;
the runner is shared; a backend is a configuration file.

## The design, in the parts that constrain changes

### Three verdicts, and one non-verdict

| | Meaning |
| --- | --- |
| `PASS` | Written and read back unchanged |
| `REJECT` | Refused at ingest, with an error |
| `ALTER` | Accepted, then quietly different — or never queryable |
| `N/A` | **Not a verdict.** The backend could not be asked. Never counted with the others. |

`REJECT` is a nuisance: something failed and you were told. `ALTER` is the
reason the project exists — nothing errors, the dashboard renders, the number is
wrong. Accepting a write and then losing the record is `ALTER`, not a fourth
verdict: from the caller's side, silently discarding data and silently changing
it are the same failure.

Do not add a verdict. A conformance defect that alters no data is reported in
the detail line, not by widening `ALTER`, which would blunt the one verdict the
project needs to keep sharp.

### `N/A` exists to stop one fact becoming many findings

A backend that does not speak a case's encoding is not failing it. Before `N/A`,
pointing the runner at a protobuf-only store produced five identical rejections,
which looks like five findings and is one fact. Whenever a limitation would
produce a row of identical failures, it belongs in `N/A` with its reason.

### Controls

A case marked `control: true` runs first. If it does not pass, every other row
in the suite is `N/A` with the reason, because none of them says anything about
the backend. A failing control usually means the adapter is wrong.

A control cannot catch everything: `minimal-record` carries a service name, so
it could not detect a read-back query that assumed one. A case that deliberately
omits a field is what exposes an adapter that assumes it.

### Normalisation, and the line it must not cross

An adapter may **rename** where a field lives and **strip** fields the backend
adds. It may not rewrite a value, coerce a type, or reorder anything a
specification orders.

If a check only passes once you normalise a value, you have found a divergence
and started to hide it. There is deliberately nowhere in the adapter schema to
rewrite a value.

The same rule governs typed comparison. `as: timestamp` compares two instants
and they are equal only when they name the same nanosecond — never by rounding
both sides to the coarser precision, which would hide exactly the divergence the
check exists to find.

### Adjudication

1. **The specification says so.** Cite the clause. Done.
2. **The specification is silent but a reference implementation is the de facto
   definition.** Mark the check `basis: de-facto` and name the reference.
3. **Genuinely ambiguous.** Do not ship a verdict. Use `match: present`, which
   records what each store did without judging it.

Two implementations disagreeing is not a rule. A check moves from `present` to
`exact` only when a rule exists, not when the disagreement gets interesting.

### Do not tune a backend to fit the corpus

If a default setting produces a divergence, that is the finding. The one
exception is an accommodation to the machine rather than to the corpus — a disk
watermark, a start-up delay — and it is recorded in the adapter with the
measurement behind it.

## Building and running

```sh
cargo test                       # 122 tests, no network, no Docker
cargo run -- up   --backend loki # start a backend from its adapter
cargo run -- run  --backend loki --suite otlp-logs
cargo run -- down --backend loki

# every column, starting and stopping each container itself
cargo run -- matrix --suite otlp-logs --manage \
  --backends parseable,openobserve,quickwit,victorialogs,loki

cargo run -- render results/<date>/otlp-logs/matrix.json
```

`run` takes `--url` if you started the backend yourself; without it, the port
comes from the adapter.

## Adding a check

1. **Find the reason first.** A clause that says MUST, MUST NOT or optional; a
   divergence observed between two implementations; or a filed bug. A check that
   cannot cite why it exists does not go in — it fails for reasons nobody cares
   about, maintainers learn to ignore the results, and the project becomes a toy.
2. Write `cases/<protocol>/<id>.yaml` with a `rule` block quoting the source and
   `basis: spec` or `basis: de-facto`. If you cannot pick one, it is not ready.
3. Put the payload beside it in its native format, so it can be sent with
   `curl`. That is the difference between a bug report a maintainer acts on and
   one they must reproduce first.
4. Choose the shape honestly: `exact` asserts, `present` records. Use `present`
   wherever the specification does not settle the answer.
5. Run it against **at least two** backends and record both verdicts verbatim in
   `notes:`, with versions.

A check that passes everywhere is still worth having. A check that fails
everywhere usually means the check is wrong.

For a query-semantics check, write the expectation by running the query against
the reference implementation and copying what it returned — never from reasoning
about what it ought to do.

## Adding a backend

Confirm **everything** against a running container. Not from documentation, not
from memory. That rule has caught an adapter error every time it has been
applied.

1. Start the container, read the version and note the JSON pointer to it.
2. Ingest one record with a known run key.
3. Search until exactly one hit comes back. Do not proceed until it does.
4. From that hit, write down the pointer to every field the corpus names.
5. Fill in the adapter from what you wrote down. Pin the image tag you pulled.
6. Run the suite. Read the control first — if it is `ALTER`, your mapping is
   wrong, not the backend.
7. Record every verdict in each case's `notes:` with the version.

Put the date and what was confirmed in a comment at the top of the adapter. An
adapter that quietly stops exercising the real path turns a column into a false
claim.

Declare `teardown` so a rerun starts clean, and `setup`/`setup_verify` if the
store needs something to exist before a write. Verify the precondition rather
than assuming it took effect: a store can answer "already exists" to a create
issued while a delete is still in flight.

## Reporting a divergence

Findings are worth more filed than tabulated.

- File in the backend's own tracker, not here.
- Make it reproduce **without this tool installed** — the payload and the exact
  request. For a protobuf endpoint, carry the payload as base64.
- Quote what the specification requires.
- Say plainly whether you reproduced it or are relaying a report.
- Include the control that rules out the obvious alternative explanation. "Your
  store dropped my record" is worth little without evidence it was not merely
  slow.
- Then link the issue from the check's `rule.observed`.

Write it the way you would want one written about your own code. Most
divergences are a reasonable reading of an ambiguous specification rather than
carelessness. Where you are unsure whether a behaviour is documented, ask rather
than assert.

## What CI enforces

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
- Every case cites a rule with a basis (`tools/check_corpus.py`)
- Every adapter pins its image and can be started
- `cargo audit`

## Never

- A score, a percentage, a badge or a ranking anywhere the project controls. A
  backend failing ten cosmetic checks is not worse than one silently dropping
  metrics, and a single number invites exactly that comparison.
- A backend's name in `src/`. If a store needs special handling, the adapter
  schema is missing something.
- `latest` in a committed adapter.
- A verdict published where the specification is silent and implementations
  disagree.
