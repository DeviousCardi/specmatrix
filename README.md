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
| `src/` | The runner. Sends cases, reads back, compares, decides verdicts. |
| `backends/` | Per-backend adapters: endpoints, auth, read-back queries. |
| `docs/` | Design, the backend roster, and how to add to either. |

The corpus is the asset. The runner is plumbing and could be rewritten in a
weekend; a corpus of checks that each trace to a line of a specification takes
much longer to build and is what makes the results worth citing.

## Status

Not yet published. Two protocols, six backends and twenty-three checks run
unattended from one command; the write-up and the upstream filings that have to
precede it are not done, and `docs/ROADMAP.md` is explicit that a framework with
no findings gets read once and forgotten.

There are findings. Among them: one store accepts a record with an int64 body,
discards it, and reports `rejected_log_records: 0`; one replaces invalid UTF-8
in a log body without saying so; one writes a sentence about its own data model
into the field that holds the log line; two answer 200 to a record they will not
keep, one of them saying nothing at all. Each is recorded in the case that found
it, with the version and the exact request.

Every one of those is unfiled, and `CONTRIBUTING.md` requires filing before
publishing: a maintainer should learn about a finding from their own tracker,
not from a comparison table.

See [`docs/DESIGN.md`](docs/DESIGN.md) and [`docs/BACKENDS.md`](docs/BACKENDS.md)
for how the checks and the roster are decided.

## Neutrality

The results are only worth reading if they are not for sale.

- Checks derive from specifications, never from one implementation's behaviour.
- Every backend gets the same corpus.
- Failures are published for every backend, including ones the maintainers
  contribute to.

A comparison written by an interested party is an advertisement. This should not
become one.

## Licence

Apache-2.0.
