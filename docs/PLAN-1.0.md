# Plan: 0.3 to 1.0, and after

This continues [`PLAN-0.2.md`](PLAN-0.2.md). Do not start it until the 0.2
launch checklist is complete and the first matrix is published. Every rule
from that plan still holds: follow in order, each step ends with a check,
anything marked *confirm by hand* is unverified and must be checked against a
running container before it enters an adapter.

Target state, from `docs/ROADMAP.md`:

| Release | Adds | Backends | Checks |
| --- | --- | --- | --- |
| 0.3 | Prometheus remote-write, OTLP metrics | 9 | ~45 |
| 1.0 | Loki push, OTLP traces, CI action, quarterly reruns | 12 | ~80 |
| after | Vendor self-certification, upstreaming the corpus | | |

The backend count is a ceiling, not a target. A column is added only when it
claims a protocol the corpus covers, runs unattended from one container, and
someone will keep it upgraded.

---

# 0.3 — Metrics

Silent alteration does the most damage here. A wrong log line is one wrong
line; a wrong counter is a wrong graph and a wrong alert.

## Part A — Remote-write encoding

Remote-write is snappy-compressed protobuf and nothing else. There is no JSON
form on the wire, so the corpus needs a JSON form of its own that the runner
encodes.

### A1. The payload format

Define `remote-write-json`, the shape the existing
`cases/remote-write/histogram-nan-count.yaml` already refers to as
`histogram-nan-count.pb.json`:

```json
{
  "timeseries": [
    {
      "labels": [
        {"name": "__name__", "value": "http_requests_total"},
        {"name": "specmatrix_run", "value": "{{ run_key }}"}
      ],
      "samples": [
        {"value": 12, "timestamp": "{{ now_ms }}"}
      ]
    }
  ]
}
```

Rules for the format, written into `docs/TEST-CASES.md`:

- `value` is a JSON number, or one of the strings `"NaN"`, `"+Inf"`, `"-Inf"`,
  `"-0.0"`. JSON cannot carry these as numbers and they are the point of
  several cases.
- `timestamp` is milliseconds, as remote-write defines it, and may be a
  template variable.
- Labels are written in the order given. The runner does not sort them; a
  case that wants unsorted labels sends unsorted labels.
- Every series carries a `specmatrix_run` label so read-back can find it.

### A2. Encoder

1. Declare `prost` types for `prometheus.WriteRequest`, `TimeSeries`,
   `Label` and `Sample` **by hand**, with `#[derive(prost::Message)]` and
   explicit tags, in `src/remote_write.rs`. Do not vendor the proto and
   generate them: `prost-build` needs `protoc` on the build machine, which
   contradicts Part E's check that the matrix runs from a clean machine with
   only Docker and Rust installed. These four messages are the whole of
   remote-write 1.0's write path; if a case later needs metadata or exemplars,
   add the field with its tag rather than taking on a code generator.

   ```rust
   #[derive(Clone, PartialEq, prost::Message)]
   pub struct WriteRequest {
       #[prost(message, repeated, tag = "1")]
       pub timeseries: Vec<TimeSeries>,
   }

   #[derive(Clone, PartialEq, prost::Message)]
   pub struct TimeSeries {
       #[prost(message, repeated, tag = "1")]
       pub labels: Vec<Label>,
       #[prost(message, repeated, tag = "2")]
       pub samples: Vec<Sample>,
   }

   #[derive(Clone, PartialEq, prost::Message)]
   pub struct Label {
       #[prost(string, tag = "1")]
       pub name: String,
       #[prost(string, tag = "2")]
       pub value: String,
   }

   #[derive(Clone, PartialEq, prost::Message)]
   pub struct Sample {
       #[prost(double, tag = "1")]
       pub value: f64,
       #[prost(int64, tag = "2")]
       pub timestamp: i64,
   }
   ```

   Verified before this plan was written: these encode, snappy-compress and
   decode back with `NaN` intact, with no `protoc` anywhere in the build.
   Add `snap` (raw block format, not framed; remote-write uses block).
2. `src/remote_write.rs`: parse the JSON form, map the special strings to
   `f64::NAN`, `f64::INFINITY`, `f64::NEG_INFINITY`, `-0.0`, encode, compress.
3. Headers on send: `Content-Type: application/x-protobuf`,
   `Content-Encoding: snappy`,
   `X-Prometheus-Remote-Write-Version: 0.1.0`.
4. Unit tests: each special value survives the JSON to protobuf step with the
   right bit pattern; `-0.0` keeps its sign; label order is preserved.

**Check:** `cargo test` passes and a hand-decoded payload, via
`protoc --decode`, shows the values sent.

### A3. Read-back for metrics

A metrics store answers PromQL, not "give me the record". Read-back for the
`remote-write` protocol is an instant query:

```yaml
readback:
  request: GET /api/v1/query?query={{ series }}{specmatrix_run="{{ run_key }}"}&time={{ now_s }}
  records: /data/result
  fields:
    value: /value/1
    labels: /metric
```

`{{ series }}` comes from `expect.readback.series` in the case. `now_s`
is seconds since the epoch and must be added to `vars_for` alongside the
variables in `PLAN-0.2.md` A1, which does not list it. The runner
already has `present`; this protocol also needs `absent`, because a stale
marker must make a series disappear from query results. Add
`match: absent`, which passes when no result carries the run key after the
poll timeout and fails as `ALTER` when one does.

Value comparison uses `as: float` (new): `"NaN"` equals `NaN`, `"+Inf"` equals
`+Inf`, and `-0.0` is compared by bit pattern, not numerically, since the
difference between `0` and `-0` is exactly what one case tests.

**Check:** unit tests for `absent` and for `as: float` including the signed
zero.

## Part B — Prometheus as reference, then the first real case

### B1. Reference column

```sh
docker run -d --name specmatrix-prometheus -p 9090:9090 \
  prom/prometheus:v2.54.0 \
  --config.file=/etc/prometheus/prometheus.yml \
  --web.enable-remote-write-receiver
```

*Confirm by hand:* `POST /api/v1/write` accepts the encoded payload with
204; `GET /api/v1/query` finds it by the run label within a few seconds;
`GET /api/v1/status/buildinfo` carries the version at `/data/version`.

Adapter `backends/prometheus.yaml` with protocol `remote-write` and
`formats: [remote-write-protobuf]`.

The name matters and is not cosmetic. The runner's eligibility check compares
a case's `send.format` against this list verbatim, so `remote-write-json` in
A1 and `remote-write-1` here would make every remote-write case read `N/A`
without sending anything. Use the same vocabulary as the OTLP suites: the case
declares `format: remote-write-json` (the encoding the *file* is written in)
and `encodings: [remote-write-protobuf]` (what it may be sent as); the adapter
declares `formats: [remote-write-protobuf]` (what it accepts on the wire).

### B2. The control case

`cases/remote-write/minimal-gauge.yaml`, `control: true`: one series, one
finite sample, `match: exact`, `on: [{field: value, as: float}, labels]`.

**Check:** `PASS` on Prometheus. Nothing else is read until it is.

### B3. The seeded cases

Both come from `docs/TEST-CASES.md` and both were reproduced against Vector.

1. `histogram-nan-count` already exists. Write its payload: a histogram whose
   `_count`, `_sum` and `_bucket` series all carry `NaN` at one timestamp,
   plus `unrelated_gauge` with a finite value in the same request. Expect the
   gauge `present` and, in a second `readback` entry, the histogram `absent`.
   The runner must support a list under `readback`; add it.
2. `histogram-nan-count-finite-sum`: `_count` is `NaN`, `_sum` and the
   buckets are finite. Reference behaviour decides what is right here, and
   the spec is silent, so `basis: de-facto`, `reference: prometheus/<version>`.
   Run it on Prometheus and copy what it does into `expect`. The Vector
   finding, a fabricated `count: 0`, is an `ALTER` against that.

**Check:** both cases have verdicts on Prometheus recorded in `notes:`, and
`histogram-nan-count` shows the two failure modes distinctly when run
against a backend that has them.

## Part C — Metrics corpus to about twenty

Every case cites the remote-write 1.0 specification, the Prometheus data
model, or a filed bug. Candidates with the clause known:

| Case | Clause | Shape |
| --- | --- | --- |
| `stale-marker-hides-series` | Senders mark staleness with a NaN sample; a receiver MUST treat it as the end of the series | `absent` after the marker |
| `stale-marker-then-resume` | A finite sample after a stale marker restarts the series | `present` with the new value |
| `positive-infinity-bucket` | The `+Inf` bucket is required on every histogram | `exact`, `as: float` |
| `negative-zero-gauge` | IEEE 754 sign preserved | `exact` by bit pattern |
| `counter-u64-max` | `f64` cannot hold `2^64 - 1`; what the store does with `18446744073709551615` | `present`, report the value |
| `float-full-precision` | A value that only survives at 53 bits | `exact`, `as: float` |
| `unsorted-labels` | Labels MUST be sorted by name; receivers MAY reject | `accepted-or-rejected`, then `present` with labels compared as a set |
| `duplicate-label-names` | Label names MUST be unique | `expect.ingest: rejected` |
| `empty-label-value` | An empty value is equivalent to an absent label | `present`, labels compared without it |
| `invalid-label-name` | Label names match `[a-zA-Z_][a-zA-Z0-9_]*` | `rejected` |
| `metric-name-utf8` | Prometheus 3 permits UTF-8 names; 2.x does not | `present`; version-dependent, record both |
| `out-of-order-samples-one-series` | Samples within a series MUST be in timestamp order | `accepted-or-rejected`, then `present` |
| `out-of-order-series-in-batch` | No ordering requirement across series | `exact` on both |
| `timestamp-far-future` | Out of the store's window | `accepted-or-rejected`, then `present`; accept-then-absent is `ALTER` |
| `timestamp-negative` | Before the epoch | `present` |
| `same-timestamp-different-value` | Duplicate sample at one timestamp | `present`; report which survived |
| `empty-write-request` | Zero timeseries | `accepted` |
| `series-with-no-samples` | Zero samples on a series | `accepted` |
| `exemplar-on-counter` | Exemplars are optional; a receiver MUST NOT fail on them | `accepted`, then `present` on the sample |
| `metadata-only-request` | Metadata without samples | `accepted` |

Write only the ones whose clause you can quote. Run each on Prometheus
first; a case that fails on the reference is a wrong case.

**Check:** about twenty remote-write cases, all green or explained on
Prometheus, `notes:` filled.

## Part D — Three more metrics columns

Nine backends total means three new ones. In this order, one at a time,
hand-confirmed:

- **VictoriaMetrics** (`victoriametrics/victoria-metrics`): write at
  `/api/v1/write`, query at `/api/v1/query`, version from
  `/metrics` line `vm_app_version`. Single binary. *Confirm by hand* whether
  it keeps `NaN` stale markers or drops them at ingest, which is itself a
  finding.
- **Grafana Mimir** (`grafana/mimir`): single-binary mode with
  `-target=all`. Write at `/api/v1/push` with `X-Scope-OrgID: specmatrix`,
  query at `/prometheus/api/v1/query` with the same header. The adapter's
  `headers` block carries it on both.
- **GreptimeDB** (`greptime/greptimedb`): write at
  `/v1/prometheus/write`, query at `/v1/prometheus/api/v1/query`. Also
  accepts OTLP metrics, so it gets that suite too.

Thanos Receive needs a second container to query. It waits until a store
earns a column by the roster rules.

Run the full remote-write suite on each. Read the control first. File every
`ALTER` upstream before recording it.

**Check:** four remote-write columns. At least one divergence is reproduced
against a backend the adapter was not tuned around. If all four agree on
everything, say so; it is a result.

## Part E — OTLP metrics

Parseable already has an `otlp-metrics` ingest block. OpenObserve,
GreptimeDB and VictoriaMetrics accept it too. OTLP metrics carry the same
special values as remote-write but in a different shape, and the interesting
question is whether a store that accepts both treats them the same.

1. Extend `src/otlp.rs` to read metric fields: the first data point of the
   first metric, with `asDouble`, `asInt`, `count`, `sum`, `bucketCounts`,
   `explicitBounds`.
2. Cases, each mirroring a remote-write case so the two suites can be read
   side by side: `gauge-nan`, `gauge-negative-zero`, `sum-int64-max`,
   `histogram-no-buckets`, `histogram-inf-bound`, `exponential-histogram`,
   `data-point-without-attributes`, `empty-scope-metrics`.
3. Read-back per backend is whatever query returns one data point by the
   `specmatrix.run` attribute. *Confirm by hand* for each.

**Check:** about eight OTLP-metrics cases across four columns. 0.3 totals:
nine backends, about forty-five checks. Rerun `specmatrix matrix`, publish
`results/<date>/`, write up the metrics findings.

---

# 1.0 — Enough to be cited

## Part F — Loki push API

De facto protocol; Loki itself is the reference column and already exists
from 0.2.

1. Format `loki-json`: the push body as documented, `streams[].stream` for
   labels and `streams[].values` as `[timestamp_ns, line]` pairs, with
   optional structured metadata as a third element. Every stream carries
   `specmatrix_run` as a label.
2. Read-back: `GET /loki/api/v1/query_range` with the selector
   `{specmatrix_run="{{ run_key }}"}`, records at `/data/result`, the line at
   `/values/0/1`, the timestamp at `/values/0/0`.
3. Cases, citing Loki's documented behaviour or a filed bug:
   `minimal-line` (control), `out-of-order-within-stream`,
   `timestamp-older-than-window`, `duplicate-line-same-timestamp`,
   `label-value-empty`, `label-name-invalid`, `structured-metadata-roundtrip`,
   `line-invalid-utf8`, `line-embedded-nul`, `timestamp-nanosecond-precision`,
   `stream-with-no-values`, `empty-push`.
4. Receivers claiming the API, hand-confirmed: VictoriaLogs at
   `/insert/loki/api/v1/push`, OpenObserve at `/api/default/loki/api/v1/push`.

**Check:** three Loki-push columns, about twelve cases, control green on
every column before anything else is read.

## Part G — OTLP traces

Specification-based, like logs.

1. Extend `src/otlp.rs` for spans: `traceId`, `spanId`, `parentSpanId`,
   `name`, `kind`, `startTimeUnixNano`, `endTimeUnixNano`, `status`,
   `attributes`, `events`, `links`.
2. Read-back is by trace id, which the runner generates per run as sixteen
   random bytes and renders as `{{ trace_id_hex }}`. That is the run key for
   this protocol.
3. Two new backends, the only ones needed to reach twelve:
   - **Jaeger** (`jaegertracing/all-in-one`): OTLP at `/v1/traces` on 4318,
     read-back `GET /api/traces/{{ trace_id_hex }}` on 16686.
   - **Grafana Tempo** (`grafana/tempo`): OTLP at `/v1/traces`, read-back
     `GET /api/traces/{{ trace_id_hex }}` on 3200, which returns 404 until
     the trace is flushed, so the poll timeout is long.
   Quickwit and OpenObserve also accept traces and get the suite.
4. Cases, each citing the OTLP or trace-semantics specification:
   `minimal-span` (control), `span-without-parent`, `parent-in-other-batch`,
   `end-before-start`, `zero-duration`, `span-id-all-zero` (invalid per
   spec), `trace-id-all-zero` (invalid), `status-code-unset-with-message`,
   `event-timestamp-outside-span`, `link-to-unknown-trace`,
   `attribute-int64-max`, `name-empty`, `kind-unspecified`,
   `dropped-attributes-count-nonzero`, `resource-without-service-name`.

**Check:** four traces columns, about fifteen cases. Twelve backends total.
Corpus at about eighty. Every case has a `notes:` line per eligible backend.

## Part H — CI action

A backend maintainer should be able to run the suite on every commit without
cloning this repository.

1. Publish the runner as a release binary for Linux x86_64 and arm64 on each
   tag, built by a workflow in this repository.
2. `action.yml` at the repository root, a composite action taking
   `backend` (path to an adapter file in the caller's repository or a name
   from this one), `suite`, `url`, and `version`. It downloads the binary
   matching `version`, runs, writes `matrix.json`, and prints the table into
   the job summary.
3. It fails the job only on a harness error. Verdicts do not fail the job;
   the maintainer decides which ones matter with a small `allow:` list in
   their adapter file, listing case ids they have read and accepted with a
   one-line reason each. The action prints the allowed ones in a separate
   section so they stay visible.
4. Dogfood it: this repository's own CI runs the action against every
   adapter in `backends/` on every pull request that touches `cases/` or
   `backends/`.

**Check:** a pull request to this repository that adds a wrong case goes red
on the reference column; a pull request from a fork can run the action
against one backend with no secrets.

## Part I — Quarterly reruns

A matrix without a date is a claim about the past that reads as a claim about
the present.

1. A scheduled workflow, first day of each quarter, runs
   `specmatrix matrix` and opens a pull request adding
   `results/<date>/matrix.json` and the rendered page.
2. Before each rerun, a maintainer bumps every image tag to the current
   release and re-confirms each adapter by hand against the checklist in
   `docs/BACKENDS.md`. That is the cost of a column, and it is the reason the
   roster is capped.
3. The page gains a history: each cell links to its previous verdicts, so a
   fix upstream is visible as a cell changing from `ALTER` to `PASS` with the
   version it changed at. That is the strongest evidence the project can
   offer that the findings are acted on.
4. A backend whose adapter cannot be re-confirmed in a quarter is shown with
   its last date and a note, never silently carried forward.

**Check:** two consecutive quarterly pages exist and at least one cell
changed between them with the version recorded.

## Part J — Governance, before calling it 1.0

The results are only worth reading if they are not for sale. Write this down
before anyone has a reason to ask.

1. `GOVERNANCE.md`: who maintains, how a maintainer is added, and the rule
   that a maintainer employed by or paid by a vendor in the matrix declares
   it in the file and does not adjudicate cases where that vendor is the
   reference or the subject.
2. `docs/ADJUDICATION.md`: the precedence from `DESIGN.md`, made
   operational. An issue template `adjudication.md` for "two backends
   disagree and the specification is silent", which is the only way a case
   moves from `present` to `exact`. The issue records the upstream question
   filed with the specification owners and its answer.
3. A `divergence.md` issue template for maintainers of a backend to dispute
   a verdict. Every dispute is answered with the payload, the request and
   the reference result, and the outcome is recorded in the case's `notes:`.
4. Funding, if any, is listed in `README.md` with the amount and source.

**Check:** the four files exist, and one adjudication has been run through
the template end to end, with the timestamp-precision case as the obvious
first candidate.

## Part K — The 1.0 release

Checklist, all required:

- [ ] Twelve columns, each with a version and a hand-confirmation date
      within the last quarter
- [ ] About eighty cases, every one quoting its rule
- [ ] Every `ALTER` filed upstream and linked
- [ ] Every `present` case has an open or closed adjudication issue
- [ ] Release binaries for two architectures
- [ ] The action works from a fork with no secrets
- [ ] Two quarterly pages published
- [ ] Governance and adjudication documents in place
- [ ] A write-up per protocol family: logs, metrics, traces

Tag `v1.0.0`. Update `README.md` status to say the matrix is maintained
quarterly and link the latest page.

---

# After 1.0

Only if there is traction, measured by the two things in `docs/ROADMAP.md`
under "what would tell you to stop": findings are acted on upstream, and the
project has stayed neutral.

## Part L — Vendor self-certification

1. A `CERTIFY.md` explaining how a vendor runs the action in their own CI,
   publishes their `matrix.json`, and links it from their documentation.
2. The published page gains a "self-reported" column style for results a
   vendor submitted from their own CI, visually distinct from results this
   project produced, with the commit and version they ran.
3. Submission is a pull request adding `results/self/<vendor>/<date>/`. The
   pull request is accepted when the JSON validates and the adapter used is
   in this repository or is committed alongside. No result is edited.

## Part M — Upstreaming the corpus

The natural home for protocol conformance checks is with the protocol.

1. Offer the remote-write cases to `prometheus/compliance`, which already
   holds sender and PromQL compliance suites and has no receiver suite. Send
   the cases, the JSON payload format, and the encoder, under their licence.
2. Offer the OTLP cases to the OpenTelemetry specification or proto
   repository as receiver conformance fixtures. Payloads are already plain
   OTLP JSON, which is the reason they were kept that way.
3. If either accepts, this repository keeps running the matrix and consumes
   the cases from upstream. Giving the corpus away is the success condition,
   not a loss.

## Part N — Knowing when to stop

Reread the stop conditions in `docs/ROADMAP.md` at each quarterly rerun and
answer them in the pull request description:

- Can the round trip still be made fair as the roster grows?
- Were the divergences filed since the last rerun acted on?
- Does every column still run without a relationship with its vendor?

If any answer is no for two consecutive quarters, say so on the page and stop
publishing rather than continue with results that no longer mean what the
README says they mean.

---

## What not to do, from 0.3 onward

- Do not add a column for a backend nobody has committed to keeping upgraded.
- Do not let a `present` case stay `present` forever; every one gets an
  adjudication issue, even if the issue stays open.
- Do not accept a self-certified result that was edited after the run.
- Do not add a score, a percentage, a badge or a ranking anywhere the
  project controls. Vendors may compute one from the JSON; the project does
  not.
- Do not write more design documents.
