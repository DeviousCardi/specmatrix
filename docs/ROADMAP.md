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

### Status

0.1 is **not** complete against its own exit criterion: no check has yet failed
against a backend whose adapter was not tuned around it, because the second
backend could not be reached. The idea is not disproved — the round trip works
and produced real results on one backend — but the second column has to wait for
the encoding work above rather than be forced now.
