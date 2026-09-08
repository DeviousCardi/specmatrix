**Title:** An empty OTLP export is answered 500

**Repository:** quickwit-oss/quickwit
**Version:** 0.8.2 (`quickwit/quickwit:0.8.2`), `QW_ENABLE_OTLP_ENDPOINT=true`
**Reproduced first-hand:** yes, on 2026-09-08.

## What happens

An export carrying no records — an `ExportLogsServiceRequest` with an empty
`resource_logs`, which encodes to zero protobuf bytes — is answered:

```
HTTP/1.1 500 Internal Server Error
{"message": "error when ingesting payload: status: Internal, message: \"\", details: [], metadata: MetadataMap { headers: {} }"}
```

The inner message is empty, so there is nothing for an operator to act on.

## Reproduction

```sh
docker run -d --name qw -p 7280:7280 \
  -e QW_ENABLE_OTLP_ENDPOINT=true quickwit/quickwit:0.8.2 run
until curl -sf localhost:7280/health/livez >/dev/null; do sleep 1; done; sleep 10

curl -s -X POST localhost:7280/api/v1/otlp/v1/logs \
  -H 'Content-Type: application/x-protobuf' --data-binary '' -w '\nHTTP %{http_code}\n'
```

Note the `sleep 10`. Quickwit creates `otel-logs-v0_7` shortly after
`/health/livez` begins answering, and an ingest before that answers `500`
`index otel-logs-v0_7 not found` — a different error, and not this one. The
behaviour above is on a warm node.

## Why this is worth reporting

`resource_logs` is a repeated field and may be empty. Collectors flush on a
timer, so an export with nothing in it is ordinary traffic rather than a client
error: the OpenTelemetry Collector's OTLP exporter will send one whenever a
flush interval elapses with no data.

A 5xx tells the client the failure is the server's and that the request should
be retried, so a collector with nothing to send retries an empty export
indefinitely, and every one of those is an error in its logs.

Four other stores were checked with the same export and all four answer 200.

If Quickwit intends to refuse empty exports, a 4xx with a message would at least
stop the retry loop; accepting them as a no-op would match what the other
implementations do.

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The check is
`cases/otlp-logs/empty-batch.yaml`.
