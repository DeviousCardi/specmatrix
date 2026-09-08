# Next steps: finishing 0.1

Follow in order. Each step ends with a check that must pass before the next
step starts. If a check fails, stop and fix it; do not skip ahead.

State at the start: the runner builds, `cargo test` passes, three OTLP-logs
checks pass against Parseable v2.9.4, and nothing is committed.

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

## Step 6 — Start Quickwit and confirm the endpoints by hand

Do not write the adapter from documentation. The Parseable adapter was wrong
about `severity_text` until a real record was inspected. Do the inspection
first.

```sh
docker run -d --name specmatrix-quickwit -p 7280:7280 \
  -e QW_ENABLE_OTLP_ENDPOINT=true \
  quickwit/quickwit:0.8.2 run
```

Wait for it, then confirm each of the following and write down what you see:

```sh
# 1. Version. Note the exact JSON path the version string sits at.
curl -s http://localhost:7280/api/v1/version

# 2. The OTLP logs index exists. Expect otel-logs-v0_7 in the list.
curl -s http://localhost:7280/api/v1/indexes | python3 -m json.tool | grep index_id

# 3. Ingest one record by hand, with a fixed run key.
sed 's/{{ run_key }}/manual-check/' cases/otlp-logs/minimal-record.json \
  | curl -s -X POST http://localhost:7280/api/v1/otlp/v1/logs \
      -H 'Content-Type: application/json' --data-binary @-

# 4. Wait a few seconds, then search for it. Quickwit commits on a timer.
sleep 5
curl -s -X POST http://localhost:7280/api/v1/otel-logs-v0_7/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "attributes.specmatrix.run:manual-check"}' | python3 -m json.tool
```

If step 4 returns zero hits, try the query `"*"` with no filter and inspect
how the attribute is actually stored, then adjust the query until the record
comes back by its run key. Do not proceed until it does.

From the hit, write down the JSON pointer to each of: the body string, the
severity text, the timestamp, and the attributes. The expected layout is
`/body/message`, `/severity_text`, `/timestamp_nanos`, `/attributes`, but use
what the response shows, not this list.

**Check:** a search by run key returns exactly one hit, and you have the four
pointers written down.

---

## Step 7 — Write the Quickwit adapter

`backends/quickwit.yaml`, filling in the values confirmed in Step 6:

```yaml
# Adapter for Quickwit.
#
# Endpoints and field paths confirmed by hand against quickwit 0.8.2 on
# <date>. Re-confirm when bumping the image.

name: quickwit
version_from:
  request: GET /api/v1/version
  field: <pointer from step 6.1, e.g. /build/version>

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
    ingest:
      request: POST /api/v1/otlp/v1/logs
      headers:
        Content-Type: application/json
    readback:
      request: POST /api/v1/otel-logs-v0_7/search
      body:
        query: "attributes.specmatrix.run:{{ run_key }}"
      records: /hits
      poll:
        interval_ms: 1000
        timeout_ms: 30000
      fields:
        body: <pointer from step 6, e.g. /body/message>
        severityText: <pointer, e.g. /severity_text>
        severityNumber: /severity_number
        timeUnixNano: <pointer, e.g. /timestamp_nanos>
        traceId: /trace_id
        spanId: /span_id

normalise:
  drop_fields: []
```

Quickwit indexes on a commit timer, so the poll timeout is longer than
Parseable's. If the runner needs `auth.kind: none` handled and does not, add
it in `authenticate` in `src/runner.rs`.

**Check:** `cargo run -- run --backend quickwit --suite otlp-logs --url http://localhost:7280`
runs without a runner error.

---

## Step 8 — Read the Quickwit results with the control rule

Read the table in this order and nothing else:

1. **`minimal-record` first.** If it is not `PASS`, the adapter is wrong. Fix
   the adapter and rerun. Do not read the other rows until it passes.
2. **`schema-url-omitted`.** Any verdict here is now about Quickwit.
3. **`empty-batch`.** Same.
4. **`body-invalid-utf8`.** Same. Compare with the Parseable verdict.
5. **`timestamp-nanosecond-precision`.** Compare the stored value with
   Parseable's. If they differ in precision, you have a divergence between two
   implementations, which is a permitted reason for the check to become
   `exact`. Do not promote it yet; note it.

Record every verdict verbatim in each case's `notes:` field with the Quickwit
version.

**Check:** `minimal-record` is `PASS` on both backends.

---

## Step 9 — Decide whether 0.1 is done

0.1 is done when a check that should fail does fail against a backend whose
adapter was not tuned around it. Answer these in writing, in
`docs/ROADMAP.md` under a new heading `## 0.1 result`:

- Which checks differ between Parseable and Quickwit, and how.
- Whether any difference was hidden or created by the adapter's field mapping.
  If a value had to be rewritten for a check to pass, that is a divergence
  and the mapping must be reverted.
- Whether the round trip can be made fair. This is the stop condition in the
  roadmap; answer it honestly.

Then commit:

```sh
git add -A
git commit -m "Quickwit adapter and 0.1 result

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01URoG3r23WhxGPjnotL4tdU"
```

**Check:** two backends, five checks, all verdicts recorded, `0.1 result`
written.

---

## What not to do during these steps

- Do not add a third backend.
- Do not add container orchestration, teardown, or version recording to the
  runner. They matter for a published matrix, not for finding out whether the
  idea works.
- Do not add a check that does not cite a rule, a divergence, or a bug.
- Do not change a field mapping to make a check pass. Change it only when a
  hand inspection shows the value is intact under a different name.
- Do not write more design documents.

---

## Teardown when finished

```sh
docker rm -f specmatrix-parseable specmatrix-quickwit
```
