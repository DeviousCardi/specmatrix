**Title:** OTLP log record with an int64 body is accepted, discarded, and reported as fully successful

**Repository:** quickwit-oss/quickwit
**Version:** 0.8.2 (`quickwit/quickwit:0.8.2`), single node, `QW_ENABLE_OTLP_ENDPOINT=true`
**Reproduced first-hand:** yes, on 2026-09-08.

## What happens

A log record whose `body` is an `AnyValue` holding `int_value` is accepted with
`HTTP 200`. The response says `rejected_log_records: 0`. The record never
becomes queryable.

A record identical except for a `string_value` body, sent seconds later to the
same index, is retrievable — so this is specific to the int64 body rather than a
slow commit or an unhealthy index.

## Reproduction

No tooling beyond `curl`, `base64` and `python3` is needed. The payload below is
a 123-byte `ExportLogsServiceRequest` whose single record has
`body.int_value = 9223372036854775807` and an attribute `specmatrix.run` set to
`issue-repro`.

```sh
docker run -d --name qw -p 7280:7280 \
  -e QW_ENABLE_OTLP_ENDPOINT=true quickwit/quickwit:0.8.2 run

# Quickwit creates otel-logs-v0_7 shortly after /health/livez answers, and the
# write path refuses for about a second after that. Wait before sending.
until curl -sf localhost:7280/health/livez >/dev/null; do sleep 1; done; sleep 10

base64 -d > /tmp/int64.pb <<'B64'
CnkKHgocCgxzZXJ2aWNlLm5hbWUSDAoKc3BlY21hdHJpeBJXCgwKCnNwZWNtYXRyaXgSRwkV9WLc9WTTGBAJGgRJTkZPKgoY//////////9/Mh8KDnNwZWNtYXRyaXgucnVuEg0KC2lzc3VlLXJlcHJvWRX1Ytz1ZNMY
B64

curl -s -X POST localhost:7280/api/v1/otlp/v1/logs \
  -H 'Content-Type: application/x-protobuf' --data-binary @/tmp/int64.pb
sleep 10
curl -s 'localhost:7280/api/v1/otel-logs-v0_7/search?query=attributes.specmatrix.run:issue-repro'
```

Observed:

```
{ "partial_success": { "rejected_log_records": 0, "error_message": "" } }
HTTP 200

{ "num_hits": 0, "hits": [], ... }
```

The same steps with a `string_value` body return `num_hits: 1`.

## Why this looks like a bug rather than a limitation

`LogRecord.body` is an `AnyValue`, and `AnyValue` may hold `int_value`; nothing
in `opentelemetry-proto` restricts a body to a string. If Quickwit cannot store
an int64 body, OTLP provides the channel for saying so:
`ExportLogsServiceResponse.partial_success` carries `rejected_log_records` and
`error_message`, and a receiver that drops records is expected to report them
there.

The difficulty is not that the record is dropped — it is that the response
states that nothing was dropped. A client that reads the response, parses
`partial_success` and checks the count is told the write succeeded completely,
so there is no signal available to it at any point.

## Would a `partial_success` count be enough?

From this project's side, yes — a caller could then see the loss and act on it.
Storing the value would be better still, but reporting the rejection would move
this from silent data loss to a documented limitation.

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The check is
`cases/otlp-logs/body-int64-max.yaml`; the rule it cites is the proto3 JSON
mapping's encoding of int64 as a decimal string, which exists so that values
past 2^53 survive the encoding.
