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

```
$ specmatrix run --backend parseable --suite otlp-logs

  otlp-logs/minimal-record                  PASS
  otlp-logs/resource-without-attributes     PASS
  otlp-logs/schema-url-omitted              REJECT   400 missing field `schemaUrl`
  otlp-logs/empty-batch                     PASS
  otlp-logs/body-invalid-utf8               ALTER    bytes replaced with U+FFFD

  5 checks, 3 pass, 1 reject, 1 alter
```

## What is in this repository

| Path | Contents |
| --- | --- |
| `cases/` | The corpus. One file per check, each citing the rule it tests. |
| `src/` | The runner. Sends cases, reads back, decides verdicts. Not written yet; see the roadmap. |
| `backends/` | Per-backend adapters: endpoints, auth, read-back queries. |
| `docs/` | Design, the backend roster, and how to add to either. |

The corpus is the asset. The runner is plumbing and could be rewritten in a
weekend; a corpus of checks that each trace to a line of a specification takes
much longer to build and is what makes the results worth citing.

## Status

Early. The design and the backend roster are settled enough to build against;
see [`docs/DESIGN.md`](docs/DESIGN.md) and [`docs/BACKENDS.md`](docs/BACKENDS.md).

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
