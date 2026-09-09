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

**Pin the current release, and check the date.** A column measured on a build a
year old is a claim about the past printed as a claim about the present, and the
divergence it reports may already be fixed. Before recording anything, look up
the store's latest release and pin that. This has already mattered: a NaN
finding against GreptimeDB v0.17.2 was fixed upstream months before it was
measured.

**Search upstream before writing a finding down.** Every divergence in this
corpus so far had prior art, and it changed what was worth saying in all three
cases — one was a known, intentional trade-off with an open issue tracking the
gap; one was already fixed in a release newer than the pin; one had a merged
pull request that turned out to cover a different code path. Link what you find
in `rule.observed` and say how the store's answer relates to it.

For a query-semantics check, write the expectation by running the query against
the reference implementation and copying what it returned — never from reasoning
about what it ought to do.

### Payload formats

A payload is kept in a format a maintainer can read and send by hand, and the
runner encodes it to whatever the backend accepts. `send.format` names the
format the file is written in, `send.encodings` lists what it may be sent as,
and an adapter's `formats:` says what it accepts on the wire.

- `otlp-json` — real OTLP JSON, sendable with `curl` as it stands.
- `es-ndjson` — a real `_bulk` body.
- `loki-json` — the push body as Loki itself documents it:
  `streams[].stream` for labels and `streams[].values` as `[timestamp_ns, line]`
  pairs, with optional structured metadata as a third element. There is no
  specification for this API; Loki defines it by what it does, which is why
  `basis: de-facto, reference: grafana-loki/<version>` cites Loki's own
  behaviour rather than a document.
- `remote-write-json` — a JSON form of a Prometheus `WriteRequest`, because the
  protocol has no JSON on the wire and a corpus of snappy-compressed protobuf
  would be a corpus nobody can read or diff. Its rules:
  - `timeseries[].labels[]` is a list of `{name, value}` pairs, sent **in the
    order written**. The specification requires senders to sort them, and
    whether a receiver enforces that is a check — so the encoder must not
    helpfully sort them.
  - `samples[].timestamp` is milliseconds, and may be a template variable.
  - `samples[].value` is a JSON number, or one of the strings `"NaN"`,
    `"StaleNaN"`, `"+Inf"`, `"-Inf"`, `"-0.0"`. JSON cannot carry these as
    numbers and they are the point of several checks. A numeric string like
    `"12"` is refused, so a typo cannot pass as data.
  - `"StaleNaN"` is the reserved payload `0x7ff0000000000002` and is **not** the
    same as `"NaN"`. Prometheus ends a series on the first and stores the
    second. Conflating them reports every store that keeps a NaN as ignoring a
    marker it was never sent.
  - Every series carries a `specmatrix_run` label so read-back can find it.

Two things the remote-write wire format cannot do, so do not write checks that
assume otherwise: it cannot carry `-0.0` (proto3 omits a scalar equal to its
default, and `-0.0 == 0.0`), and an empty `WriteRequest` encodes to zero bytes.

### Read-back that is not a lookup

Some protocols answer a query rather than returning a record. For those:

- `series:` names which series a check asserts on, and becomes
  `{{ series_selector }}` — a selector the runner builds, because only it knows
  both the metric name and the adapter's run-key label, and because a name that
  is not a legacy identifier needs PromQL's quoted form.
- `at:` sets `{{ query_time_s }}`, the instant to ask about. It defaults to
  unset, and an unset parameter is not sent — so the query means "now". Only a
  check that deliberately timestamps a sample away from the present needs it.
- `match: absent` asserts a record is **not** there. It waits the full poll
  timeout before concluding, and it needs a witness: put an ordinary series in
  the same request and assert it `present` **first**. A store that has not
  caught up answers "nothing here" to everything, and an absence check on its
  own passes for the wrong reason. VictoriaMetrics defaults to a 30s
  `search.latencyOffset`, which is exactly long enough to do it.

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

`unsupported:` lists cases this adapter cannot put to this backend, each with a
reason that is printed in the cell. It is only ever an inability of the query
language or of the adapter — GreptimeDB's PromQL has no quoted-name selector, so
a metric named `a.b.c` cannot be named in a query even though the data is there.
It is **never** a way to exclude a case the store fails. If you are reaching for
it because a verdict is inconvenient, you are writing a false column.

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
