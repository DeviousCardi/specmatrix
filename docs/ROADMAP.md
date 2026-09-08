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
