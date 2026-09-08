**Title:** A log entry outside the retention period is answered 200 with an empty body and silently discarded

**Repository:** VictoriaMetrics/VictoriaMetrics (VictoriaLogs)
**Version:** v1.52.0 (`victoriametrics/victoria-logs:v1.52.0`), default `-retentionPeriod=7d`
**Reproduced first-hand:** yes, on 2026-09-08.

## What happens

An OTLP log record timestamped thirty days ago is answered:

```
HTTP/1.1 200 OK
Content-Length: 0
```

and never becomes queryable. A zero-length body is a valid but empty
`ExportLogsServiceResponse`: it carries no `partial_success`, so it states that
the whole batch was accepted.

The documentation for `-retentionPeriod` says entries with timestamps outside
the retention "are also rejected during data ingestion". VictoriaLogs therefore
considers the record rejected, and the caller is told it succeeded.

## Reproduction

```sh
docker run -d --name vl -p 9428:9428 victoriametrics/victoria-logs:v1.52.0
until curl -sf localhost:9428/health >/dev/null; do sleep 1; done

# One record, timestamped 30 days before this payload was generated.
base64 -d > /tmp/stale.pb <<'B64'
CuIBCh4KHAoMc2VydmljZS5uYW1lEgwKCnNwZWNtYXRyaXgSlgEKEwoKc3BlY21hdHJpeBIFMC4xLjASVgkAPsT0DjDKGBAJGgRJTkZPKhkKF3NwZWNtYXRyaXggc3RhbGUgcmVjb3JkMh8KDnNwZWNtYXRyaXgucnVuEg0KC2lzc3VlLXN0YWxlWQA+xPQOMMoYGidodHRwczovL29wZW50ZWxlbWV0cnkuaW8vc2NoZW1hcy8xLjIxLjAaJ2h0dHBzOi8vb3BlbnRlbGVtZXRyeS5pby9zY2hlbWFzLzEuMjEuMA==
B64

curl -s -D - -o /dev/null -X POST localhost:9428/insert/opentelemetry/v1/logs \
  -H 'Content-Type: application/x-protobuf' --data-binary @/tmp/stale.pb
sleep 3
curl -sG localhost:9428/select/logsql/query \
  --data-urlencode 'query=specmatrix.run:=issue-stale' | wc -l   # 0
```

A control rules out other causes: the same record dated three days ago, inside
the retention window, answers `200` and is found.

## Why this is worth reporting

Retention is configuration and differs between deployments; that is not the
issue. The issue is the channel used to report the outcome. OTLP defines
`ExportLogsServiceResponse.partial_success`, carrying `rejected_log_records` and
`error_message`, precisely so a receiver that will not keep part of a batch can
say so on the success path. Returning an empty response asserts the opposite.

A collector shipping a backlog after an outage — the case where old timestamps
arrive in bulk — loses the whole backlog with no error at any layer.

Populating `partial_success` with the count and a message naming the retention
period would make this visible without changing what is stored.

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The check is
`cases/otlp-logs/timestamp-outside-ingest-window.yaml`. Two other stores were
checked with the same payload: one refuses it with `400` and a message naming
the oldest acceptable timestamp, and one accepts it and reports the discard in
`partial_success`.
