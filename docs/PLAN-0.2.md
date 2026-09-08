# Plan: 0.2, the first public artefact

Follow in order. Each step ends with a check. If a check fails, stop and fix
it; do not skip ahead. Anything marked *confirm by hand* is from memory or
documentation, not verified here, and must be checked against a running
container before it goes into an adapter. That rule caught three adapter
errors in 0.1 and it stays.

Start state: 0.1 complete. Three adapters (Parseable, OpenObserve, Quickwit
as `N/A`), five OTLP-logs checks, one recorded divergence, runner at about
1,100 lines with tests.

Target state, from `docs/ROADMAP.md`: the Elasticsearch `_bulk`/`_search`
protocol with Elasticsearch as reference, six backends, about twenty-five
checks, a published matrix page with versions and a date, and a write-up.

The order below puts findings before infrastructure. The page is last because
a page with nothing on it is not worth building.

---

## Part A — Pay the four debts from 0.1

These were found in 0.1 and each one blocks something in 0.2. Do them first,
one commit each.

### A1. Timestamps in payloads are relative, not fixed

A payload dated 2025 ages out of every store's ingest window. The corpus must
not test the fixture.

1. Add template variables in `vars_for` in `src/runner.rs`:
   `now_ns` (nanoseconds since epoch at send time), `now_us`, `now_ms`,
   `now_ns_fractional`, `now_minus_1d_ns` and `now_minus_30d_ns`.
2. Replace every fixed `timeUnixNano` and `observedTimeUnixNano` in
   `cases/otlp-logs/*.json` with `"{{ now_ns }}"`, taken from a nanosecond
   clock rather than a millisecond one.
3. `timestamp-nanosecond-precision` uses `"{{ now_ns_fractional }}"`, **not**
   `now_ns`. A live nanosecond clock is non-deterministic in exactly the way
   this check cannot tolerate: land on a whole millisecond and the check
   silently reports no precision loss, which reads as a store that preserved
   nanoseconds. `now_ns_fractional` is the current time truncated to the
   second plus a fixed remainder of `123456789`, so it is fresh enough for any
   ingest window and its low digits are known. The check can then say exactly
   which digits a store dropped.
3. The expected value for a `timeUnixNano` comparison is the rendered value,
   which the runner already gets by rendering the payload before parsing it.
4. Revert `ZO_INGEST_ALLOWED_UPTO` in `backends/openobserve.yaml` to the
   default. The adapter should not tune the store to fit the corpus.

**Check:** all five checks give the same verdicts as recorded in
`docs/ROADMAP.md` against both Parseable and OpenObserve at default settings.

### A2. A case for accept-then-discard

OpenObserve answered 200 to a record outside its ingest window and stored
nothing. That is the failure class the project exists for, and it now has a
citation: the 0.1 result.

Create `cases/otlp-logs/timestamp-outside-ingest-window.yaml`:

```yaml
id: otlp-logs/timestamp-outside-ingest-window
protocol: otlp-logs
title: A record the store will not keep is refused, not silently dropped

rule:
  basis: spec
  spec: opentelemetry/otlp/1.0
  section: otlphttp-response
  text: >
    On success the server MUST respond with HTTP 200. If the server cannot
    process the request it MUST respond with an appropriate HTTP 4xx or 5xx
    status. A store with a retention or ingest window that will not keep a
    record has not processed it, and answering 200 tells the client the data
    is safe when it is gone.
  observed:
    - docs/ROADMAP.md, 0.1 result, OpenObserve v0.92.2 at default settings

send:
  format: otlp-json
  body: cases/otlp-logs/timestamp-outside-ingest-window.json

expect:
  ingest: accepted-or-rejected
  readback:
    match: exact
    on: [body]

notes: >
  Either verdict at ingest is conformant: REJECT tells the client. What is not
  conformant is 200 followed by absence, which the runner reports as ALTER.
```

The payload is `minimal-record.json` with both timestamps set to
`"{{ now_minus_30d_ns }}"`. Add that variable in A1's `vars_for`.

Runner: support `ingest: accepted-or-rejected`. A rejection is `PASS` with
the status in the detail. An acceptance continues to read-back as normal, so
accepted-then-absent is `ALTER`, which the runner already does.

**Check:** OpenObserve at default settings gives `ALTER`, detail
`accepted (200) but never became queryable`. Parseable gives whatever it
gives; record it in `notes:`. File the OpenObserve finding upstream per
`CONTRIBUTING.md` and add the issue link to the case.

### A3. Typed comparison

Parseable returns a millisecond string, OpenObserve a microsecond integer.
`sent X, read back Y` is honest but the two rows cannot be compared to each
other.

Extend `expect.readback.on` to accept either a bare field name or
`{field: timeUnixNano, as: timestamp}`. Supported `as` values for 0.2:

| `as` | Parses | Compares |
| --- | --- | --- |
| `timestamp` | integer ns/us/ms by magnitude, RFC 3339 string | as an instant, and reports the precision the backend kept |
| `integer` | JSON number, numeric string | as i128 |
| `string` | anything | exact |

The verdict rule does not change: `exact` still fails on any difference, and
`present` still reports. What changes is the detail line, which for a
timestamp now reads `sent 1755000000123456789 ns, read back 1755000000123 ms
(precision: milliseconds)`. That line is what makes two backends comparable
in one table.

This is not normalisation, and the comparison rule is what keeps it from
becoming normalisation: under `match: exact` two values are equal only when
they denote the same instant **at the finer of the two precisions**. A
nanosecond value and a millisecond value are never equal, so a store that
dropped digits fails an `exact` check rather than passing one that coerced
both sides to milliseconds first. Coercing to a common precision would hide
precisely the divergence 0.1 found, and `docs/DESIGN.md` forbids it.

Nothing is rewritten; the runner reads a value the backend chose to store,
says what it found, and reports the precision alongside it.

Unit tests: each `as` value, each accepted input shape.

**Check:** `timestamp-nanosecond-precision` shows precision `milliseconds`
for Parseable and `microseconds` for OpenObserve.

### A4. Protobuf encoding, and Quickwit gets verdicts

Cases stay JSON files. The runner transcodes when the adapter accepts
`otlp-protobuf` and not `otlp-json`.

1. Add `opentelemetry-proto` with the `logs`, `gen-tonic-messages` and
   `with-serde` features, plus `prost`. Confirm the feature names against the
   crate version you pin; they have changed between releases.
2. **Remove the eligibility early-return first, or none of this runs.**
   `src/runner.rs` currently returns `N/A` before the payload is even read:

   ```rust
   if !protocol.formats.is_empty() && !protocol.formats.contains(&case.send.format) {
       return Ok(self.result(case, Verdict::NotApplicable, ...));
   }
   ```

   Quickwit declares `formats: [otlp-protobuf]` and every case declares
   `format: otlp-json`, so this fires for all five and the transcode below is
   dead code. Replace it with a check that asks whether the case can be
   *converted* to something the adapter accepts, and only reports `N/A` when
   it cannot:

   ```rust
   let offered = case.send.encodings();   // defaults to [case.send.format]
   let Some(encoding) = choose_encoding(&protocol.formats, &offered) else {
       return Ok(self.result(
           case,
           Verdict::NotApplicable,
           format!("encoding {} not accepted by this backend", offered.join(" or ")),
       ));
   };
   ```

   with `send.encodings` a new optional list on the case, defaulting to
   `[format]`, and `choose_encoding` returning the first offered encoding the
   adapter accepts (or the first offered one when the adapter declares none,
   so existing adapters keep working). `body-invalid-utf8` declares no
   `encodings`, so it stays JSON-only and correctly reads `N/A` on a
   protobuf-only backend.
3. In `run_case`, once an encoding is chosen: if it differs from the case's
   own format, parse the rendered payload into `ExportLogsServiceRequest` via
   serde and encode with prost. Send with
   `Content-Type: application/x-protobuf`. Keep the comparison against the
   **rendered JSON**, never the wire bytes — a protobuf body cannot be read
   field by field and the question is what the backend did to what the case
   sent.
4. If the parse fails, the verdict is `N/A` with detail
   `payload cannot be encoded as protobuf: <error>`. This is the correct
   outcome for `body-invalid-utf8`: a protobuf string field cannot carry
   those bytes, so the check does not apply.
5. Quickwit read-back. *Confirm by hand*, in this order, writing down each
   response:

   ```sh
   docker run -d --name specmatrix-quickwit -p 7280:7280 \
     -e QW_ENABLE_OTLP_ENDPOINT=true quickwit/quickwit:0.8.2 run
   # Send minimal-record through the runner with --verbose, then:
   curl -s -X POST http://localhost:7280/api/v1/otel-logs-v0_7/search \
     -H 'Content-Type: application/json' -d '{"query": "*"}' | python3 -m json.tool
   ```

   From a hit, write down the pointers for body, severity text, timestamp and
   the run-key attribute. Then find the query that returns exactly one hit by
   run key. Only then fill in `readback` in `backends/quickwit.yaml`.

**Check:** Quickwit shows `PASS` on `minimal-record`, a verdict on each of
the other cases, and `N/A` with the encoding reason on `body-invalid-utf8`.
Record every verdict in `notes:`. Commit.

---

## Part B — The Elasticsearch protocol

This is where the seed cases from `docs/TEST-CASES.md` live, and it needs a
second case shape: not "send one record, read it back" but "load a small
dataset, run a query, compare which documents come back".

### B1. Elasticsearch as the reference column

```sh
docker run -d --name specmatrix-elasticsearch -p 9200:9200 \
  -e discovery.type=single-node -e xpack.security.enabled=false \
  -e ES_JAVA_OPTS='-Xms512m -Xmx512m' \
  docker.elastic.co/elasticsearch/elasticsearch:8.15.0
```

Pin the exact version after the first pull. *Confirm by hand:* `GET /`
returns the version at `/version/number`; `POST /_bulk` with
`Content-Type: application/x-ndjson`; `POST /<index>/_search`; and that a
document is searchable after `POST /<index>/_refresh`.

Adapter `backends/elasticsearch.yaml` with a new protocol block `es-bulk`:

```yaml
protocols:
  es-bulk:
    formats: [ndjson]
    ingest:
      # The index goes in the path. A bulk action line with no `_index` and no
      # index in the URL is refused with action_request_validation_exception,
      # so this is not a stylistic choice.
      request: POST /{{ suite_stream }}/_bulk
      headers:
        Content-Type: application/x-ndjson
    refresh:
      request: POST /{{ suite_stream }}/_refresh
    readback:
      request: POST /{{ suite_stream }}/_search
      records: /hits/hits
      fields:
        body: /_source/body
    query:
      request: POST /{{ suite_stream }}/_search
      hits: /hits/hits
      # The marker is a field inside the document, not `_id`. See B2.
      marker: /_source/doc
```

`refresh` is new and optional: an adapter may declare a request the runner
sends after ingest and before read-back. Parseable and OpenObserve do not
need it. Elasticsearch does.

### B2. The query case shape

Add to `src/case.rs`:

```yaml
id: es-query/must-not-absent-field
protocol: es-bulk
kind: query
title: must_not on a field matches documents where the field is absent

rule:
  basis: de-facto
  reference: elasticsearch/8.15.0
  text: >
    In Elasticsearch a must_not clause on a term excludes documents where the
    term matches and retains every other document, including those where the
    field does not exist. A backend claiming the Elasticsearch query API must
    match this, not SQL semantics where a NULL comparison is neither true nor
    false.
  observed:
    - https://github.com/quickwit-oss/quickwit/issues/6474

dataset: cases/es-query/absent-field.ndjson
query:
  body: cases/es-query/must-not-absent-field.query.json
expect:
  hits: [with-field-other, without-field]
  order: any
```

`dataset` is an `_bulk` body whose documents each carry a `doc` field holding
a stable marker, sent once per case through the adapter's ingest, then
`refresh`, then `query`. The runner collects `doc` from each hit and compares
the set (or sequence, when `order: as-listed`) with `expect.hits`.

**The marker is a document field, not `_id`.** B4 notes that whether a backend
honours `_id` on bulk is itself a finding — and if it does not, every query
case in this suite fails for that one reason, producing eight findings out of
one fact. That is the mistake 0.1 caught with Quickwit's content-type, and the
`N/A` machinery exists to stop it. A field inside `_source` is carried by every
store that accepts the document at all, so a query case measures the query
rather than the id policy. `es-query/id-preserved` in Part D tests `_id`
separately, where it is the subject rather than the instrument.

Verdicts for `kind: query`: bulk rejected is `REJECT`; hits equal is `PASS`;
hits differ is `ALTER` with detail `expected {a, b}, got {a}`. There is no
"present" mode; a query case is always exact, because the expectation was
taken from the reference.

Rule for writing `expect.hits`: run the query against Elasticsearch first and
copy what it returned. Never write the expectation from reasoning about what
Elasticsearch should do. Record the reference version in `rule.reference`.

### B3. The first eight query cases

Each cites a filed bug, an observed divergence, or a documented reference
behaviour. Build the dataset once, `absent-field.ndjson`, with four documents:

| `doc` | `level` | `service` |
| --- | --- | --- |
| `with-field-match` | `error` | `api` |
| `with-field-other` | `info` | `api` |
| `without-field` | (absent) | `api` |
| `null-field` | `null` | `api` |

Every document also carries `specmatrix.run`, so a rerun cannot read another
run's rows.

Cases, in order:

1. `must-not-absent-field`: quickwit#6474.
2. `exists-query`: `exists: {field: level}`. Reference excludes both
   `without-field` and `null-field`.
3. `wildcard-presence`: `query_string: "level:*"`. quickwit#6475.
4. `sort-missing-last`: sort on `level` ascending. Where the missing
   documents land is reference-defined.
5. `sort-missing-first`: same with `missing: _first`.
6. `terms-agg-partial-field`: aggregate on `level`; compare bucket keys and
   counts, so `expect` gains an `aggregations` form. Skip if it costs more
   than a day; it can be case nine.
7. `match-all-count`: control for this suite. `match_all` returns all four.
   Mark `control: true`.
8. `term-on-null`: `term: {level: null}` is a 400 in the reference. Expect
   `ingest: accepted`, `query: rejected`. Add `expect.query` with
   `accepted`/`rejected` for this.

**Check:** all eight pass on Elasticsearch. That is the reference and it must
be green by construction; a red row here means the case is wrong.

### B4. Second and third Elasticsearch-API columns

Quickwit and OpenObserve both claim it. *Confirm by hand* for each: the bulk
path, whether `_id` is honoured or replaced, the search path, and whether a
refresh call exists or a wait is needed.

- Quickwit: `POST /api/v1/_elastic/_bulk` and
  `POST /api/v1/_elastic/<index>/_search`. Quickwit requires an index to exist
  with a doc mapping first; the adapter's `setup` block (new, optional, one
  request sent once per suite) creates it. Whether `_id` survives is a
  finding in itself.
- OpenObserve: `POST /api/default/_bulk` and
  `POST /api/default/<stream>/_search` or the SQL endpoint. Whether it
  implements the query DSL at all decides whether this column exists.

Run the suite on both. Read `match-all-count` first. Record verdicts in
`notes:`. File each `ALTER` upstream with the dataset, the query, the
reference result, and the observed result.

**Check:** three columns for `es-bulk`, at least one `ALTER` filed upstream
with a link in the case.

---

## Part C — Six backends

Four exist after Part B: Parseable, OpenObserve, Quickwit, Elasticsearch.
Two more, chosen because they claim protocols the corpus already covers and
run from one container:

- **VictoriaLogs** (`victoriametrics/victoria-logs`): OTLP logs at
  `/insert/opentelemetry/v1/logs`, Elasticsearch bulk at
  `/insert/elasticsearch/_bulk`, read-back through LogsQL at
  `/select/logsql/query`. *Confirm by hand* whether OTLP JSON is accepted and
  how attribute keys are stored.
- **Grafana Loki** (`grafana/loki`): OTLP logs at `/otlp/v1/logs`, read-back
  through `/loki/api/v1/query_range` with a LogQL selector on the run key.
  Loki stores the body as the line and attributes as structured metadata;
  *confirm by hand* which attributes become labels and which do not, because
  that decides the read-back query.

For each, in this order: start the container, confirm by hand, write the
adapter, run `minimal-record`, fix until it passes, run the suite, record
verdicts. Do not start the second until the first is committed.

**Check:** six adapters, each with a note at the top saying what was
confirmed by hand and on what date. Every case's `notes:` has a line per
backend.

---

## Part D — Corpus to about twenty-five checks

After Part B there are about fourteen. The rest come from
`docs/TEST-CASES.md`'s categories, and each must cite a clause. Candidates
with the clause already known, in priority order:

| Case | Clause | Expected shape |
| --- | --- | --- |
| `otlp-logs/duplicate-attribute-keys` | Attribute keys MUST be unique (OTel spec, common/attributes) | `present`; report which value survived |
| `otlp-logs/int64-max-attribute` | OTLP JSON encodes int64 as a decimal string | `exact` on the attribute, `as: integer` |
| `otlp-logs/timestamp-zero` | `time_unix_nano` of 0 means unknown | `present`; report what the store did |
| `otlp-logs/body-kvlist` | body is an AnyValue and may be a kvlist | `exact` on body |
| `otlp-logs/resource-without-attributes` | resource.attributes is repeated and may be empty | `exact` on body |
| `otlp-logs/severity-number-out-of-range` | SeverityNumber enum 1 to 24; 0 is unspecified | `present` |
| `otlp-logs/embedded-nul-in-body` | valid UTF-8, includes U+0000 | `exact` on body |
| `otlp-logs/long-single-token` | no clause; observed limit differences count once two backends differ | `present` |
| `es-query/terms-agg-partial-field` | reference behaviour | `exact` on aggregation |
| `es-query/id-preserved` | `_id` in bulk is the document id | `exact` on hits |
| `es-query/text-vs-keyword-term` | term on a text field does not match in the reference | `exact` on hits |

Write only the ones whose clause you can quote in `rule.text`. If a clause
cannot be found, the case waits. Run each new case against all six backends
before committing it, and record the verdicts in `notes:`.

**Check:** about twenty-five cases, every one with a `rule` block that quotes
its source, every one with a `notes:` line per backend.

---

## Part E — Run the whole matrix unattended

Only now does orchestration earn its place. The runner must be able to
produce the entire matrix from one command, or the page in Part F will be a
one-time screenshot.

### E1. Container lifecycle in the runner

The `container:` block already exists in every adapter. Implement it:

- `specmatrix up <backend>`: `docker run` from the block, wait for `ready`,
  print the version from `version_from`.
- `specmatrix down <backend>`: remove the container.
- `specmatrix run` keeps taking `--url`, so a hand-started backend still
  works.

Use the Docker CLI through `std::process::Command`, not a Docker client
crate. It is one command and one poll.

### E2. `setup` and `teardown`

Send the adapter's `setup` request once before a suite and `teardown` once
after. Teardown removes the stream or index so a rerun starts clean. Confirm
by running the suite twice in a row against each backend and getting the
same verdicts both times.

### E3. `specmatrix matrix`

One command: for each adapter in `backends/`, for each suite in `cases/`,
`up`, run, `down`. Output `results/<date>/matrix.json` holding, per cell,
the verdict, the detail, the backend version, the image, and the timestamp.
Any `N/A` carries its reason. Any harness error is recorded as such and
never as a verdict.

### E4. Version recording

Every result row records the backend version as reported by the backend, not
the image tag. Where they disagree, both are recorded and the disagreement
is noted.

**Check:** `specmatrix matrix` runs from a clean machine with only Docker and
Rust installed, finishes without intervention, and produces a `matrix.json`
whose verdicts match the `notes:` in every case.

---

## Part F — Publish

### F1. The page

`specmatrix render results/<date>/matrix.json` writes a single static HTML
file: backends as columns, cases as rows, one cell per verdict with the
detail on hover or expand. The header carries the date, each backend's
version and image, the commit of the corpus, and a plain sentence that the
page is generated from the JSON beside it.

No score. No ranking. No colour for `N/A` that resembles pass or fail. Each
case row links to its YAML and each `ALTER` links to the upstream issue if
one is filed.

### F2. File before publishing

Every `ALTER` on the page has an upstream issue, filed as
`CONTRIBUTING.md` describes, before the page goes up. A maintainer should
learn about a finding from their own tracker, not from a comparison table.

### F3. The write-up

The launch artefact is a post, not the repository. It leads with what was
found, states the method in a paragraph, links the page and the corpus, and
says plainly what is de facto versus specified and what remains
unadjudicated. It names nothing "worst".

### F4. Launch checklist

- [ ] Six columns with versions
- [ ] About twenty-five rows, each citing its rule
- [ ] Every `ALTER` filed upstream and linked
- [ ] Every `N/A` carries its reason
- [ ] `specmatrix matrix` reproduces the page from a clean machine
- [ ] `README.md` status updated from "Early" to the date of the first matrix
- [ ] The write-up says what is not yet adjudicated

**Check:** the checklist is complete. Then publish.

---

## What not to do during 0.2

- Do not add metrics or remote-write. They are 0.3, and the one case already
  in `cases/remote-write/` waits there.
- Do not add a seventh backend. Two more columns cost less than one
  unmaintained one.
- Do not write a check without a clause to quote.
- Do not tune a backend's settings to fit the corpus. If a default setting
  produces a divergence, that is the finding.
- Do not build the page before Part E works unattended.
- Do not write more design documents.

---

Continues in [`PLAN-1.0.md`](PLAN-1.0.md): 0.3 metrics, 1.0, and after.
