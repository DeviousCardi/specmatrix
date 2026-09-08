**Title:** A log record with no body is stored with a documentation notice as its message

**Repository:** VictoriaMetrics/VictoriaMetrics (VictoriaLogs)
**Version:** v1.52.0 (`victoriametrics/victoria-logs:v1.52.0`), defaults
**Reproduced first-hand:** yes, on 2026-09-08.

## What happens

An OTLP log record that carries no `body` is accepted, and the stored `_msg` is:

```
missing _msg field; see https://docs.victoriametrics.com/victorialogs/keyconcepts/#message-field
```

A record whose body is a `kvlist_value` — a structured body rather than a string
— produces the same string.

Nothing errors at ingest. Anything reading the stream back shows that sentence
where the application's log line would be, and a reader has no way to tell it
apart from a line the application emitted.

## Reproduction

The endpoint takes protobuf only, so the payload is given as base64. It is a
112-byte `ExportLogsServiceRequest` with one record: `severity_text` `INFO`, an
attribute `specmatrix.run` = `issue-nobody`, and no `body` field.

```sh
docker run -d --name vl -p 9428:9428 victoriametrics/victoria-logs:v1.52.0
until curl -sf localhost:9428/health >/dev/null; do sleep 1; done

base64 -d > /tmp/nobody.pb <<'B64'
Cm4KHgocCgxzZXJ2aWNlLm5hbWUSDAoKc3BlY21hdHJpeBJMCgwKCnNwZWNtYXRyaXgSPAkVC2IDeGXTGBAJGgRJTkZPMiAKDnNwZWNtYXRyaXgucnVuEg4KDGlzc3VlLW5vYm9keVkVC2IDeGXTGA==
B64

curl -s -X POST localhost:9428/insert/opentelemetry/v1/logs \
  -H 'Content-Type: application/x-protobuf' --data-binary @/tmp/nobody.pb
sleep 3
curl -sG localhost:9428/select/logsql/query \
  --data-urlencode 'query=specmatrix.run:=issue-nobody'
```

Observed: ingest answers `200`, and the query returns a record whose `_msg` is
the notice quoted above.

## Why this is worth reporting

`LogRecord.body` is an optional `AnyValue` in `opentelemetry-proto`, and proto3
omits a field that is not set, so a record with no body is conformant and
ordinary: an event fully described by its attributes and severity has no message
to carry.

The concern is not that VictoriaLogs needs a `_msg`. It is that the substituted
value is indistinguishable, to everything downstream, from content the
application produced. Four other stores were checked with the same payload:
three leave the field absent and one stores an empty string.

An empty `_msg`, or a field naming the substitution — anything a query could
filter on — would keep the diagnostic without it being mistaken for data. If the
current behaviour is deliberate, is there a documented way to distinguish a
substituted message from a real one?

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The checks are
`cases/otlp-logs/record-without-body.yaml` and `cases/otlp-logs/body-kvlist.yaml`.
