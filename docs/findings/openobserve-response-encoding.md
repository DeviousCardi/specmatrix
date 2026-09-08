**Title:** OTLP: a JSON export is answered with a protobuf body labelled `content-type: application/json`

**Repository:** openobserve/openobserve
**Version:** v0.92.2 (`openobserve/openobserve:v0.92.2`)
**Reproduced first-hand:** yes, on 2026-09-08.

## What happens

A record older than the ingest window (`ZO_INGEST_ALLOWED_UPTO`, five hours by
default) is discarded, and OpenObserve reports that properly through
`ExportLogsServiceResponse.partial_success` — `rejected_log_records: 1` with a
clear `error_message`.

The report cannot be read by the client that made the request. The export was
JSON; the response body is protobuf, and the header says:

```
HTTP/1.1 200 OK
content-type: application/json
content-length: 182
```

The body is not JSON. A conformant OTLP/JSON client parses it as JSON, fails,
and never sees the notice — so a discard that was correctly reported arrives in
a form nothing will read, and the data loss becomes silent in practice.

## Reproduction

```sh
docker run -d --name oo -p 5080:5080 \
  -e ZO_ROOT_USER_EMAIL=admin@example.com -e 'ZO_ROOT_USER_PASSWORD=Example#2026' \
  openobserve/openobserve:v0.92.2
until curl -sf localhost:5080/healthz >/dev/null; do sleep 1; done

OLD=$(( ($(date +%s) - 30*86400) * 1000000000 ))
curl -s -D - -u 'admin@example.com:Example#2026' \
  -X POST localhost:5080/api/default/v1/logs \
  -H 'Content-Type: application/json' -H 'stream-name: encdemo' \
  --data-binary '{"resourceLogs":[{"resource":{"attributes":[]},"scopeLogs":[{"scope":{},"logRecords":[{"timeUnixNano":"'$OLD'","observedTimeUnixNano":"'$OLD'","body":{"stringValue":"old"}}]}]}]}' \
  --output /tmp/resp.bin
file /tmp/resp.bin        # not JSON
xxd /tmp/resp.bin | head  # 0a b3 01 08 01 12 ... a protobuf ExportLogsServiceResponse
```

Decoded, the body is `partial_success { rejected_log_records: 1, error_message:
"Too old data, only last 5 hours data can be ingested. Data discarded. ..." }`.

## Why this is worth reporting

The OTLP/HTTP specification requires the response to use the same encoding as
the request: a JSON-encoded export is answered with a JSON-encoded response.
Here the encoding is wrong and the `content-type` header describes the encoding
that was expected rather than the one that was sent, so a client cannot even
detect the mismatch and fall back to a protobuf decode.

The behaviour underneath is good — OpenObserve does use the channel OTLP defines
for a partial discard, which several other stores do not. Emitting that same
response as JSON when the request was JSON would make it visible to the clients
it is meant for.

## A note on the same class elsewhere

For completeness, and because it is the same requirement in the other direction:
Quickwit 0.8.2 answers a **protobuf** OTLP export with a **JSON** body, also
under `content-type: application/json`. That is reported separately to that
project; it is mentioned here only so this report is not read as singling out one
implementation.

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The check is
`cases/otlp-logs/timestamp-outside-ingest-window.yaml`.
