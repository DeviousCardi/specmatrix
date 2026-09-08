# Roadmap

## 0.1 — prove the round trip

Not for publication. The point is to find out whether the idea survives contact
with two real backends.

- One protocol: OTLP/HTTP logs, JSON encoding
- Two backends: Parseable and Quickwit, both a single container
- Three checks: an omitted optional field, an empty batch, an invalid-UTF-8 body
- Output: a printed table

Done when a check that should fail does fail, against a backend you did not write
the adapter around.

If the round trip turns out to be impractical — because read-back is too
backend-specific to normalise fairly — that is worth discovering now, in a
weekend, rather than after building a plugin architecture.

## 0.2 — the first public artefact

- Add the Elasticsearch `_bulk` and `_search` protocol, with Elasticsearch itself
  as the reference column
- Six backends
- Around twenty-five checks, including the seeded `ALTER` cases
- Publish the matrix as a page, with versions and the date

**Do not launch before this.** A framework with two backends and no findings gets
read once and forgotten, and first impressions do not repeat.

The launch artefact is the write-up, not the repository: *"I tested six log
backends against the protocols they claim to support, and three of them silently
change your data."* The repository is the footnote that makes it checkable.

## 0.3 — metrics

Remote-write, where silent alteration does the most damage: a wrong log line is
one wrong line, a wrong counter is a wrong graph and a wrong alert.

- Nine backends
- Around forty-five checks
- Prometheus as the reference column

## 1.0 — enough to be cited

- Loki push API and OTLP traces
- Twelve backends
- Around eighty checks
- A CI action, so a backend can self-certify on every commit
- Quarterly re-runs, published with versions

## After 1.0, if it has traction

**Vendor self-certification.** A challenger backend has a real incentive to
publish "we pass 94% of the Elasticsearch compatibility suite, here is the 6% we
do not" — it is a sharper claim than any marketing page, and it is checkable.
Once one adopts it, others follow to avoid the comparison being made without
them.

**Upstreaming the corpus.** The natural home for protocol conformance checks is
with the protocol, not with a third party. If OpenTelemetry or Prometheus wanted
the corpus, giving it to them would be a success rather than a loss.

## What would tell you to stop

Worth deciding now, while it is cheap to be honest.

- **The round trip cannot be made fair.** If normalisation has to be so
  aggressive that everything passes, the tool cannot distinguish conformance from
  noise.
- **Nobody acts on the findings.** If the first several divergences are filed
  upstream and ignored, the results are not useful to the people best placed to
  use them.
- **It stops being neutral.** If keeping a column running depends on a
  relationship with the vendor, the results stop being worth reading, and it is
  better to say so and stop than to quietly keep publishing them.

---

## 0.1 result

Two backends were attempted. One was integrated; the second could not be, and
the reason is the most useful thing 0.1 produced.

### Which checks differ between Parseable and Quickwit

None, because the suite never reached Quickwit.

Quickwit 0.8.2 — which is also what `quickwit/quickwit:latest` resolves to —
accepts only `application/x-protobuf` on its OTLP endpoint. Every other
`Content-Type`, JSON included, is refused before the body is read:

```
$ curl -X POST localhost:7280/api/v1/otlp/v1/logs \
    -H 'Content-Type: application/json' --data-binary @minimal-record.json
{"message": "Invalid request header \"content-type\""}
```

Sending the same bytes as `application/x-protobuf` gets further and fails in
the decoder, confirming binary is the only encoding it will parse.

The OTLP specification makes JSON support a SHOULD rather than a MUST:

> Server implementations SHOULD accept OTLP/HTTP with binary-encoded Protobuf
> payload and OTLP/HTTP with JSON-encoded Protobuf payload requests on the same
> port and multiplex the requests to the corresponding payload decoder based on
> the Content-Type request header.

So this is a deviation from a recommendation, not a violation of a requirement,
and the corpus should record it that way rather than as a failure.

Verdicts recorded against Parseable v2.9.4:

| Check | Verdict | Detail |
| --- | --- | --- |
| `minimal-record` | PASS | round trip intact |
| `schema-url-omitted` | PASS | round trip intact |
| `empty-batch` | PASS | accepted |
| `body-invalid-utf8` | REJECT | 400, invalid unicode code point at column 345 |
| `timestamp-nanosecond-precision` | PASS (present) | sent `1755000000123456789`, read back `2025-08-12T12:00:00.123` |

### Was any difference hidden or created by the field mapping

Yes, once, and the control caught it.

The first run reported `severityText` as absent on every check, which read as
two `ALTER`s. Inspecting a stored record showed Parseable keeps the value under
`severity_text`; the value itself was intact. That is a rename, so the mapping
was added and the checks went green. No value was rewritten to make anything
pass, and `docs/BACKENDS.md`'s rule that a failing `minimal-record` means the
adapter is wrong is what stopped it being filed as a finding.

### Can the round trip be made fair

For a single encoding, yes. Parseable's five checks ran, compared, and produced
one recorded transformation and one rejection, all traceable to what was sent.

But 0.1 exposed a hole in the design. The corpus assumes every backend speaks
one wire encoding per protocol, and that is not true: `otlp-logs` is really
"OTLP logs, JSON-encoded", and a backend that only speaks protobuf is not
failing those checks, it is not eligible for them. The runner currently cannot
say that. Pointing it at Quickwit would produce five `REJECT`s with an identical
content-type error, which looks like five findings and is one fact.

Two consequences for 0.2, and neither is optional:

1. **Encoding belongs in the case, not the suite name.** A case should carry its
   encoding, and a backend adapter should declare which encodings it accepts, so
   the report can say `N/A — encoding not supported` instead of manufacturing
   rejections.
2. **The control rule needs a third outcome.** "Control failed, so the adapter is
   wrong" is not always true. It can also mean the backend cannot accept this
   shape of traffic at all, and conflating the two would have had me debugging a
   correct adapter.

### Which checks differ between Parseable and OpenObserve

One of five, and it is the check that was written not to judge.

| Check | Parseable v2.9.4 | OpenObserve v0.92.2 |
| --- | --- | --- |
| `minimal-record` (control) | PASS | PASS |
| `schema-url-omitted` | PASS | PASS |
| `empty-batch` | PASS | PASS |
| `body-invalid-utf8` | REJECT, 400 invalid unicode code point | REJECT, 400 invalid unicode code point |
| `timestamp-nanosecond-precision` | `"2025-08-12T12:00:00.123"` | `1755000000123456` |

Both stores refuse invalid UTF-8 rather than storing a rewritten body, which is
what the rule permits, and they agree on the three checks that assert equality.

They disagree on the timestamp. Sent `1755000000123456789`, Parseable keeps a
millisecond-precision **string** and OpenObserve keeps a microsecond **integer**.
Both discard nanoseconds, by different amounts and into different types. Anyone
moving between the two silently changes the resolution of every timestamp they
own, and nothing in either system reports it.

That disagreement is the condition the roadmap set for promoting the check from
`present` to `exact`. It is not being promoted, because promotion needs a rule
saying what a store must preserve and the OTLP specification does not give one.
Under the adjudication order in `docs/DESIGN.md` this is case 3, genuinely
ambiguous, and the answer is to record it and not ship a verdict.

### Was any verdict hidden or created by a field mapping

No, and one mapping was required in each adapter.

Parseable keeps severity under `severity_text`; OpenObserve keeps it under
`severity` and has no column for `severityNumber` at all. Both are renames, and
a check naming `severityNumber` on OpenObserve reads as absent rather than being
quietly satisfied from another column. No value was rewritten to make anything
pass.

One backend setting was changed and it is not a mapping: OpenObserve's default
`ZO_INGEST_ALLOWED_UPTO` is five hours, and the corpus payloads carry a fixed
timestamp from 2025. At the default the store answers **200 and discards the
record**, which the runner reported as an `ALTER`. That verdict was correct
about what happened and wrong about why: the cause was the fixture's age, not a
defect. Widening the window is recorded in the adapter with that reasoning.

The behaviour is worth a case of its own in 0.2. Accepting a write with 200 and
discarding it is the exact failure this project exists to surface, and it is
currently only visible because a fixture happened to trip it.

### Is the 0.1 exit criterion met

Yes. `body-invalid-utf8` produced `REJECT` against OpenObserve, whose adapter
was written before that check was ever run against it, and
`timestamp-nanosecond-precision` produced a value that differs from Parseable's.
Neither outcome was arranged by the adapter.

The weaker claim also holds: the control caught a real adapter error before it
became a finding. The first Parseable run reported two `ALTER`s that were my
`severity_text` mapping, not the backend.

### Can the round trip be made fair across stores with different models

Yes, with one qualification that 0.2 has to carry.

Two stores with unrelated storage models — Parseable on a data lake with SQL,
OpenObserve with its own columnar engine — ran the same five payloads and
returned comparable answers, using nothing but renames. Fairness did not require
normalising values, which was the stop condition in this roadmap.

The qualification is types. Parseable returned a timestamp as a string and
OpenObserve as an integer, so `sent X, read back Y` is honest but not directly
comparable across a row. A check that wants to assert on a value across stores
needs to say what type it expects, or the matrix will compare a string with a
number and call it a difference when it may not be one.

### Status

0.1 is complete. Two stores with verdicts on five checks, one store recorded as
ineligible with the reason, one real divergence found, and no verdict invented
where the specification is silent.

Carried into 0.2, in the order they were discovered:

1. **Protobuf encoding**, so Quickwit gets verdicts instead of a column of `N/A`.
2. **A case for accept-then-discard**, prompted by OpenObserve's 200 on
   out-of-window data.
3. **Types in checks**, so a string timestamp and an integer timestamp can be
   compared without pretending they are the same shape.
4. **Timestamps in payloads should not be fixed.** A corpus that ages out of a
   store's ingest window tests the fixture, not the backend.
