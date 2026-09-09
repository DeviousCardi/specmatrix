# SpecMatrix

Compatibility testing for observability backends.

Most observability products advertise compatibility with a protocol they did not
invent: an Elasticsearch-compatible search API, a Prometheus remote-write
endpoint, native OTLP ingestion. Those claims are rarely tested. They hold for
ordinary traffic and come apart at the edges, and the edges are where migrations
break.

SpecMatrix sends payloads that are unusual but valid, reads the data back, and
reports what each backend actually did with it.

## The question it answers

> If I move my logs from A to B, do my queries still return the same rows?

Today that question is answered by migrating and finding out. It should be
answerable in an afternoon.

## Three verdicts

Every check resolves to one of three outcomes. They are not equally serious.

| Verdict | Meaning |
| --- | --- |
| `PASS` | Written and read back unchanged |
| `REJECT` | Refused at ingest, with an error |
| `ALTER` | Accepted, then quietly different when read back |

`REJECT` is a nuisance: something failed, you were told, you deal with it.

`ALTER` is the reason this project exists. Nothing errors, the dashboard renders,
the number is wrong. A histogram whose count was replaced with zero looks exactly
like a histogram whose count really is zero.

## Example

A real run, abridged — eleven of the sixteen rows are omitted:

```
$ specmatrix up  --backend loki
loki ready at http://localhost:3100

$ specmatrix run --backend loki --suite otlp-logs

  loki release-3.1.x-89fe788  http://localhost:3100
  suite: otlp-logs

  otlp-logs/body-invalid-utf8                ALTER    body: invalid bytes replaced with U+FFFD
  otlp-logs/empty-batch                      PASS     204, no read-back declared
  otlp-logs/minimal-record                   PASS     204, round trip intact
  otlp-logs/timestamp-nanosecond-precision   PASS     recorded — timeUnixNano: sent
                                                      "1788883853123456789" (string, nanoseconds),
                                                      read back "1788883853123456789" (string, nanoseconds)
  otlp-logs/timestamp-outside-ingest-window  PASS     400 entry for stream has timestamp too old …

  16 checks, 15 pass, 0 reject, 1 alter, 0 n/a
  1 accepted then changed or lost — these fail silently in production
```

That `ALTER` is the point of the tool. Loki answered `204 No Content` and stored
`before-\xef\xbf\xbd\xef\xbf\xbd-after` for a body that was sent as
`before-\xff\xfe-after`: each invalid byte replaced with U+FFFD, no error, no
warning. Two other stores refuse the same payload with a 400, which is also
allowed — the rule permits replacement when a backend documents it, so the
question put upstream asks whether Loki does.

`specmatrix matrix --manage` runs a suite against several backends, starting and
stopping each container itself, and writes `matrix.json`, `matrix.md` and a
static `matrix.html` under `results/<date>/<suite>/`.

## What is in this repository

| Path | Contents |
| --- | --- |
| `cases/` | The corpus. One file per check, each citing the rule it tests. |
| `backends/` | Per-backend adapters: endpoints, auth, read-back queries. |
| `src/` | The runner. Sends cases, reads back, compares, decides verdicts. |
| `tools/` | The corpus gate CI runs. |
| [`AGENTS.md`](AGENTS.md) | The design, and how to add a check or a backend. |

The corpus is the asset. The runner is plumbing and could be rewritten in a
weekend; a corpus of checks that each trace to a line of a specification takes
much longer to build and is what makes the results worth citing.

## Status

Not yet published. Five protocols, ten backends and sixty-six checks run
unattended from one command; the write-up that has to precede publication is
not done.

Logs came first: two protocols, six stores, twenty-four checks. Metrics
followed, and they are where silent alteration does the most damage — a wrong
log line is one wrong line, a wrong counter is a wrong graph and a wrong alert.
Prometheus, Grafana Mimir, GreptimeDB and VictoriaMetrics each answer both
Prometheus remote-write and OTLP metrics, and both suites read back through the
same query, so the columns compare two ways into the same store as well as four
stores against each other.

There are findings. Among them: one store accepts a record with an int64 body,
discards it, and reports `rejected_log_records: 0`; one replaces invalid UTF-8
in a log body without saying so; one writes a sentence about its own data model
into the field that holds the log line; one answers 204 to a series carrying a
duplicate label, logs the problem, counts it, and stores nothing; one accepts a
histogram's count and sum with 200 and keeps neither. Each is recorded in the
case that found it, with the version and the exact request.

Each is filed with the project it concerns, with a reproduction that needs
nothing but `curl` — `specmatrix encode` produces the body for the protocols
that exist on the wire only as protobuf — and each is linked from the check
that found it. `CONTRIBUTING.md` requires that a maintainer learns about a
finding from their own tracker rather than from a comparison table.

Loki's push API joined the same three log stores that already answer OTLP —
Loki itself, VictoriaLogs, OpenObserve — read back through the same
per-protocol query each already had. Two things worth knowing before reading
that suite: VictoriaLogs stores an invalid-UTF-8 log line raw rather than
substituting it the way Loki does, which makes its own query response not
valid JSON — a documented-API client cannot parse what the documented API
returns for that record. And OpenObserve's push landed at
`/api/default/loki/api/v1/push` rather than the path its own merged pull
request's example used, confirmed only by trying both against a running
container rather than trusting the documentation.

Not everything that differs is a finding, and the corpus says so. A NaN sample
dropped at ingest, an exponential histogram with no representation, a metric
name that does or does not gain its unit as a suffix: those are recorded with
links to the upstream decision or tracking issue, and no new issue is filed.
Three would-be findings turned out to be this project's own bugs, and one had
been fixed upstream two months before it was measured, on a pin that was a year
old. That is why every column names the release it ran against and the date it
was confirmed.

[`AGENTS.md`](AGENTS.md) has the design and the rules a change has to satisfy.
[`CONTRIBUTING.md`](CONTRIBUTING.md) has what a pull request needs.

## Running it

Rust and Docker; nothing else.

```sh
cargo test                        # 166 tests, no network, no containers

cargo run -- up   --backend loki  # start a backend from its adapter
cargo run -- run  --backend loki --suite otlp-logs
cargo run -- down --backend loki

# every column, starting and stopping each container itself
cargo run -- matrix --suite otlp-logs --manage \
  --backends parseable,openobserve,quickwit,victorialogs,loki

cargo run -- matrix --suite remote-write --manage \
  --backends prometheus,mimir,greptimedb,victoriametrics

# the bytes a backend actually receives, for reproducing a finding by hand
cargo run -- encode cases/remote-write/minimal-gauge.json \
  --from remote-write-json --to remote-write-protobuf --out body.snappy
```

`matrix` writes `matrix.json`, `matrix.md` and a static `matrix.html` under
`results/<date>/<suite>/`. The JSON is the artefact; the page is rendered from
it and says so.

**It writes to whatever you point it at**, sends deliberately malformed payloads,
and deletes the stream or index each case uses before running it. Point it at a
store you are willing to have written to, and read
[`SECURITY.md`](SECURITY.md) first.

## Backends

| Backend | OTLP logs | Elasticsearch `_bulk` |
| --- | --- | --- |
| Parseable | ✓ | |
| OpenObserve | ✓ | ingest only — no query API |
| Quickwit | ✓ protobuf only | ✓ |
| VictoriaLogs | ✓ protobuf only | |
| Grafana Loki | ✓ | |
| Elasticsearch | | ✓ *(reference)* |

Elasticsearch is the reference for its own protocol, so it cannot fail it. That
asymmetry is a property of the method, not a result.

## Contributing

Two kinds of contribution matter most: a check that cites a real rule, and a
backend adapter someone will keep running. Start with
[`AGENTS.md`](AGENTS.md), then [`CONTRIBUTING.md`](CONTRIBUTING.md).

Every check must cite a clause, an observed divergence, or a filed bug. Checks
invented to pad the matrix make it look thorough and make it worthless.

## Neutrality

The results are only worth reading if they are not for sale.

- Checks derive from specifications, never from one implementation's behaviour.
- Every backend gets the same corpus.
- Failures are published for every backend, including ones the maintainers
  contribute to.

A comparison written by an interested party is an advertisement. This should not
become one.

## Licence

Apache-2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

Product names in `cases/`, `backends/` and `results/` belong to their owners.
Their appearance records what one version of one product did with one payload on
one date, and is not a claim about the product in general.
