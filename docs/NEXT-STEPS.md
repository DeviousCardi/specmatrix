# Next steps: finishing 0.1

Follow in order. Each step ends with a check that must pass before the next
step starts. If a check fails, stop and fix it; do not skip ahead.

Steps 1 to 5 are done and committed. Step 6 as originally written could not be
completed: Quickwit 0.8.2 refuses OTLP JSON at the `Content-Type` header, and
the runner had no way to say "not eligible" rather than "REJECT". The record is
in `docs/ROADMAP.md` under `## 0.1 result`. Steps 6 onward are rewritten to fix
that and reach the 0.1 exit criterion with a backend that does speak JSON.

---

## Step 1 — Commit what exists

Nothing is in git. Two commits, so the design and the code stay separable.

```sh
git add .gitignore LICENSE README.md CONTRIBUTING.md docs backends cases
git commit -m "Design, corpus layout, and the Parseable adapter

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01URoG3r23WhxGPjnotL4tdU"

git add Cargo.toml Cargo.lock src
git commit -m "0.1 runner: send, poll read-back, compare, verdict

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01URoG3r23WhxGPjnotL4tdU"
```

**Check:** `git log --oneline` shows two commits and `git status --short` is
empty.

---

## Step 2 — Make read-back refuse records that are not ours

`first_record` in `src/runner.rs` returns the first element of the array, or
the whole object if the response is an object. Two false positives follow:

- An error response that happens to be a JSON object counts as "present".
- A leftover record from an earlier run counts as this run's record.

Change `first_record` to take the run key and return only a record whose
serialised JSON contains it:

```rust
fn first_record(value: &Value, records_pointer: &str, run_key: &str) -> Option<Value> {
    let node = if records_pointer.is_empty() { value } else { value.pointer(records_pointer)? };
    let items: Vec<&Value> = match node {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![node],
        _ => return None,
    };
    items.into_iter().find(|r| r.to_string().contains(run_key)).cloned()
}
```

Pass `vars["run_key"]` through from `read_back`. Add a unit test: an array
holding one record without the key and one with it returns the second.

**Check:** `cargo test` passes. Rerun against Parseable (Step 5 shows the
command) and all three checks still pass.

---

## Step 3 — Make `match: present` report instead of judge

Today `present` and `exact` behave identically: both compare every field in
`on` and any difference is an `ALTER`. `present` is meant for values the
project has not adjudicated yet.

In `run_case`, after the record is found:

- If `expect.match_ == "exact"`: current behaviour.
- If `expect.match_ == "present"`: verdict `PASS`, and the detail string lists
  each field in `on` as `field: sent X, read back Y`, so the stored value is
  visible in the table without being asserted.
- Anything else: return an error naming the unsupported value.

**Check:** `cargo test` passes and the table still shows three `PASS` against
Parseable.

---

## Step 4 — Add the timestamp precision case as `present`

Parseable turns `timeUnixNano` `1755000000000000000` into
`"2025-08-12T12:00:00"`. This is a real transformation, but whether a store
must preserve nanoseconds is not settled, so the case reports and does not
judge.

Create `cases/otlp-logs/timestamp-nanosecond-precision.yaml`:

```yaml
id: otlp-logs/timestamp-nanosecond-precision
protocol: otlp-logs
title: A nanosecond timestamp is stored at full precision

rule:
  basis: spec
  spec: opentelemetry-proto/logs/v1
  section: LogRecord.time_unix_nano
  text: >
    time_unix_nano is the time the event occurred, in nanoseconds since the
    Unix epoch. The specification defines the field's precision but does not
    say what a store must preserve, so this check records the stored value
    rather than asserting it. It becomes an exact check once two
    implementations disagree or the specification is clarified.

send:
  format: otlp-json
  body: cases/otlp-logs/timestamp-nanosecond-precision.json

expect:
  ingest: accepted
  readback:
    match: present
    on: [timeUnixNano]

notes: >
  Observed: Parseable v2.9.4 stores this as a second-precision string.
  Adjudication open.
```

Create the payload by copying `minimal-record.json` and changing
`timeUnixNano` and `observedTimeUnixNano` to `"1755000000123456789"`, a value
that is only preserved at full precision. Keep the `specmatrix.run` attribute.

**Check:** the table shows four `PASS` and the detail for this case prints the
stored value.

---

## Step 5 — Add the invalid-UTF-8 case against Parseable

This is the third of the roadmap's 0.1 checks, and the one most likely to
produce a verdict other than `PASS`. Test it on the backend whose adapter is
already trusted, so any surprise is a finding rather than adapter noise.

### 5a. Runner change

`run_case` parses the sent payload with `serde_json::from_slice`, which fails
on invalid UTF-8 and produces `Null`, so every expected field reads as
`<absent>` and the detail is misleading. Change it to:

1. Try a strict parse. If it succeeds, use it.
2. Otherwise parse `String::from_utf8_lossy(&bytes)` and remember that the
   payload was lossy.
3. When the payload was lossy and a field's read-back value equals the lossy
   expected value, report `ALTER` with the detail
   `field: invalid bytes replaced with U+FFFD`. Any other difference is an
   `ALTER` with the normal detail. Equality cannot occur, because a JSON
   response cannot carry the original bytes.

Add a unit test for the lossy path.

### 5b. Payload

The payload must contain a raw invalid byte, so write it with `printf`, not an
editor:

```sh
printf '%s' '{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"specmatrix"}}]},"scopeLogs":[{"scope":{"name":"specmatrix","version":"0.1.0"},"logRecords":[{"timeUnixNano":"1755000000000000000","observedTimeUnixNano":"1755000000000000000","severityNumber":9,"severityText":"INFO","body":{"stringValue":"before-' > cases/otlp-logs/body-invalid-utf8.json
printf '\xff\xfe' >> cases/otlp-logs/body-invalid-utf8.json
printf '%s' '-after"},"attributes":[{"key":"specmatrix.run","value":{"stringValue":"{{ run_key }}"}}]}]}]}]}' >> cases/otlp-logs/body-invalid-utf8.json
```

**Check:** `file cases/otlp-logs/body-invalid-utf8.json` does not say
`UTF-8 Unicode text`, and `python3 -c "open('cases/otlp-logs/body-invalid-utf8.json','rb').read().decode()"`
fails with a decode error.

### 5c. Case

`cases/otlp-logs/body-invalid-utf8.yaml`:

```yaml
id: otlp-logs/body-invalid-utf8
protocol: otlp-logs
title: A body containing invalid UTF-8 is not silently rewritten

rule:
  basis: spec
  spec: opentelemetry-proto/common/v1
  section: AnyValue.string_value
  text: >
    Protobuf string fields must be valid UTF-8, and a conformant decoder is
    permitted to reject a payload that is not. Rejecting is a REJECT. Accepting
    and replacing the bytes is an ALTER unless the backend documents the
    replacement. Accepting and truncating the body is an ALTER regardless.
  observed:
    - https://github.com/vectordotdev/vector/issues/20462

send:
  format: otlp-json
  body: cases/otlp-logs/body-invalid-utf8.json

expect:
  ingest: accepted
  readback:
    match: exact
    on: [body]
```

### 5d. Run

Start Parseable if it is not running:

```sh
docker run -d --name specmatrix-parseable -p 8000:8000 \
  -e P_USERNAME=admin -e P_PASSWORD=admin \
  -e P_FS_DIR=/parseable/data -e P_STAGING_DIR=/parseable/staging \
  quay.io/parseablehq/parseable:v2.9.4 parseable local-store
```

```sh
cargo run -- run --backend parseable --suite otlp-logs --url http://localhost:8000
```

**Check:** five rows in the table. The invalid-UTF-8 row is `REJECT` or
`ALTER`, and its detail names what happened. Record the result verbatim in the
case's `notes:` field with the Parseable version.

Commit:

```sh
git add -A
git commit -m "Run-key matching, present-mode reporting, timestamp and invalid-UTF-8 cases

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01URoG3r23WhxGPjnotL4tdU"
```

---

## Step 6 — Encoding belongs to the case; eligibility belongs to the adapter

The suite name `otlp-logs` hides the encoding. Every case already carries
`send.format: otlp-json`; the adapter needs to say which formats it accepts,
and the runner needs a fourth outcome for "cannot be asked".

### 6a. Adapter

Add a `formats` list to each protocol in `backends/parseable.yaml`:

```yaml
protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      ...
```

In `src/backend.rs`, add `#[serde(default)] pub formats: Vec<String>` to
`Protocol`. An empty list means "not declared" and is treated as accepting
everything, so existing adapters keep working until they are filled in.

### 6b. Runner

Add `Verdict::NotApplicable`, printed as `N/A`. In `run_case`, before reading
the payload:

```rust
if !protocol.formats.is_empty() && !protocol.formats.contains(&case.send.format) {
    return Ok(self.result(case, Verdict::NotApplicable,
        format!("encoding {} not accepted by this backend", case.send.format)));
}
```

The summary line gets a separate count: `5 checks, 0 pass, 0 reject, 0 alter,
5 n/a`. `N/A` is not a verdict about the backend and must never be added into
pass or fail totals.

### 6c. Test

Unit test: a protocol declaring `[otlp-protobuf]` and a case sending
`otlp-json` yields `NotApplicable` without any request being made.

**Check:** `cargo test` passes and Parseable still shows the five results
recorded in the roadmap.

---

## Step 7 — The control rule gets a third outcome

Today a failing `minimal-record` is read as "adapter is wrong". Quickwit showed
it can also mean "backend cannot take this traffic at all". The runner should
distinguish them and stop the suite rather than print five copies of one fact.

### 7a. Case

Add `control: true` to `cases/otlp-logs/minimal-record.yaml`, and
`#[serde(default)] pub control: bool` to `Case`.

### 7b. Runner

In `run_suite`, run control cases first. Then:

| Control outcome | Meaning | Action for remaining cases |
| --- | --- | --- |
| `PASS` | Adapter and backend both work | Run them |
| `REJECT` | Backend refused an ordinary record | Mark all `N/A`, detail `control rejected: <status and first line>` |
| `ALTER` | Backend took the record, adapter cannot read it back correctly | Mark all `N/A`, detail `control altered: adapter mapping is wrong, fix it before trusting any row` |
| `N/A` | Encoding not accepted | Mark all `N/A` with the same detail |
| harness error | Runner or network problem | Stop with the error |

Only the control row shows the real verdict. The rest say `N/A` and why.

**Check:** point the runner at a port with nothing listening. Output is one
harness error, not five. Then run against Parseable and the five results are
unchanged.

---

## Step 8 — Record Quickwit as a column that cannot be asked

Write `backends/quickwit.yaml` with what was confirmed by hand, and nothing
that was not:

```yaml
# Adapter for Quickwit.
#
# Confirmed against quickwit 0.8.2 on <date>: the OTLP endpoint accepts
# application/x-protobuf only. JSON is refused at the Content-Type header
# before the body is read. The OTLP specification makes JSON a SHOULD, so this
# is a deviation from a recommendation, not a conformance failure. Read-back
# is deliberately absent until the runner can send protobuf (see 0.2).

name: quickwit
version_from:
  request: GET /api/v1/version
  field: <pointer confirmed by hand>

container:
  image: quickwit/quickwit:0.8.2
  command: ["run"]
  port: 7280
  env:
    QW_ENABLE_OTLP_ENDPOINT: "true"
  ready:
    request: GET /health/livez
    expect_status: 200

auth:
  kind: none

protocols:
  otlp-logs:
    formats: [otlp-protobuf]
    ingest:
      request: POST /api/v1/otlp/v1/logs
      headers:
        Content-Type: application/x-protobuf
```

Run it:

```sh
cargo run -- run --backend quickwit --suite otlp-logs --url http://localhost:7280
```

**Check:** five `N/A` rows, each saying the encoding is not accepted, and a
summary of `5 n/a`. No request was sent. This is the first time the report
says one fact once.

Commit:

```sh
git add -A
git commit -m "Encoding eligibility, control outcomes, and a Quickwit column marked N/A

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01URoG3r23WhxGPjnotL4tdU"
```

---

## Step 9 — Second column: OpenObserve

OpenObserve accepts OTLP/HTTP JSON and stores it, so it can reach the 0.1
exit criterion without protobuf. Confirm every endpoint by hand before writing
the adapter, exactly as Step 6 originally required.

### 9a. Start it

```sh
docker run -d --name specmatrix-openobserve -p 5080:5080 \
  -e ZO_ROOT_USER_EMAIL=admin@specmatrix.local \
  -e ZO_ROOT_USER_PASSWORD=specmatrix \
  openobserve/openobserve:latest
```

Pull `latest` once, read the version in 9b, then pin that exact tag in the
adapter. Never leave `latest` in a committed adapter.

### 9b. Confirm by hand

Write down every response. Do not go from documentation.

```sh
AUTH='admin@specmatrix.local:specmatrix'

# 1. Version. Note the JSON path.
curl -s -u "$AUTH" http://localhost:5080/api/config | python3 -m json.tool | grep -i version

# 2. Ingest one record with a fixed run key. Org is `default`; the stream
#    name goes in a header.
sed 's/{{ run_key }}/manual-check/' cases/otlp-logs/minimal-record.json \
  | curl -s -u "$AUTH" -X POST http://localhost:5080/api/default/v1/logs \
      -H 'Content-Type: application/json' -H 'stream-name: specmatrixotlplogs' \
      --data-binary @-

# 3. Wait, then search by run key. Times are microseconds since epoch.
sleep 5
NOW=$(date +%s%6N); START=$((NOW - 3600000000))
curl -s -u "$AUTH" -X POST http://localhost:5080/api/default/_search \
  -H 'Content-Type: application/json' \
  -d "{\"query\":{\"sql\":\"SELECT * FROM \\\"specmatrixotlplogs\\\" WHERE specmatrix_run = 'manual-check'\",\"start_time\":$START,\"end_time\":$NOW}}" \
  | python3 -m json.tool
```

If step 3 returns no hits, search with no `WHERE` clause and look at how the
attribute was actually stored. OpenObserve flattens attribute keys and usually
turns `.` into `_`, but confirm it from the record, then adjust the query until
exactly one hit comes back by run key. Do not proceed until it does.

From the hit, write down the pointer to: the body, the severity text, the
timestamp, and the run-key attribute.

**Check:** one hit by run key, four pointers written down, version and its
JSON path written down.

### 9c. Runner: microsecond window variables

The Parseable adapter uses `window_start` and `window_end` as RFC 3339
strings. OpenObserve wants microseconds. In `vars_for` in `src/runner.rs`, add
`window_start_us` and `window_end_us` alongside the existing ones. No other
runner change should be needed; if one is, write down what and why in the
commit message.

### 9d. Adapter

`backends/openobserve.yaml`, filled from 9b:

```yaml
# Adapter for OpenObserve.
#
# Confirmed by hand against openobserve <version> on <date>. Re-confirm when
# bumping the image.

name: openobserve
version_from:
  request: GET /api/config
  field: <pointer from 9b.1>

container:
  image: openobserve/openobserve:<pinned tag>
  port: 5080
  env:
    ZO_ROOT_USER_EMAIL: admin@specmatrix.local
    ZO_ROOT_USER_PASSWORD: specmatrix
  ready:
    request: GET /healthz
    expect_status: 200

auth:
  kind: basic
  username: admin@specmatrix.local
  password: specmatrix

protocols:
  otlp-logs:
    formats: [otlp-json]
    ingest:
      request: POST /api/default/v1/logs
      headers:
        Content-Type: application/json
        stream-name: "{{ suite_stream }}"
    readback:
      request: POST /api/default/_search
      body:
        query:
          sql: "SELECT * FROM \"{{ suite_stream }}\" WHERE <run-key column from 9b> = '{{ run_key }}'"
          start_time: "{{ window_start_us }}"
          end_time: "{{ window_end_us }}"
      records: /hits
      poll:
        interval_ms: 1000
        timeout_ms: 30000
      fields:
        body: <pointer from 9b>
        severityText: <pointer from 9b>
        severityNumber: <pointer from 9b>
        timeUnixNano: <pointer from 9b>

normalise:
  drop_fields:
    - _timestamp
```

If `start_time` and `end_time` must be JSON numbers rather than strings, the
template substitution will produce a quoted string. In that case, change the
render step in `read_back` to try parsing a rendered `"{{ ... }}"` value as a
number before falling back to a string. Write a unit test for it.

### 9e. Run and read with the control rule

```sh
cargo run -- run --backend openobserve --suite otlp-logs --url http://localhost:5080
```

Read `minimal-record` first. If it is `REJECT`, the backend cannot take the
record and the adapter is not at fault; inspect the response. If it is
`ALTER`, the field mapping is wrong; go back to 9b. Only when it is `PASS` do
the other four rows say anything about OpenObserve.

Record every verdict verbatim in each case's `notes:` field with the
OpenObserve version.

**Check:** `minimal-record` is `PASS`, five rows have verdicts, all recorded.

---

## Step 10 — Rewrite the 0.1 result

Replace the `### Status` block of `## 0.1 result` in `docs/ROADMAP.md`. Answer:

- Which checks differ between Parseable and OpenObserve, and how. The likely
  candidates are `body-invalid-utf8`, which Parseable rejects, and
  `timestamp-nanosecond-precision`, where Parseable keeps milliseconds.
- Whether any verdict was hidden or created by a field mapping.
- Whether the 0.1 exit criterion is met: a check produced a verdict other than
  `PASS` against a backend whose adapter was not tuned around it. If both
  backends agree on all five, say so plainly; that is a result too.
- Whether the round trip can be made fair across two stores with different
  storage models. This is the stop condition and gets a direct answer.

Commit:

```sh
git add -A
git commit -m "OpenObserve adapter and 0.1 result

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01URoG3r23WhxGPjnotL4tdU"
```

**Check:** two stores with verdicts, one store marked `N/A` with a reason,
`0.1 result` updated, working tree clean.

---

## Deferred to 0.2, and why

- **Protobuf encoding of OTLP.** Needed to give Quickwit real verdicts and
  needed anyway for remote-write in 0.3. Cases stay as JSON files; the runner
  decodes OTLP JSON into the generated types and re-encodes as protobuf. The
  invalid-UTF-8 case stays JSON-only and shows `N/A` on protobuf backends,
  which is correct: a protobuf string field cannot carry those bytes.
- **Promoting `timestamp-nanosecond-precision` to `exact`.** Only once two
  implementations disagree on precision. Step 10 will say whether they do.

---

## What not to do during these steps

- Do not add a fourth backend.
- Do not add container orchestration, teardown, or version recording to the
  runner.
- Do not add a check that does not cite a rule, a divergence, or a bug.
- Do not change a field mapping to make a check pass. Change it only when a
  hand inspection shows the value is intact under a different name.
- Do not write more design documents.

---

## Teardown when finished

```sh
docker rm -f specmatrix-parseable specmatrix-quickwit specmatrix-openobserve
```
