**Title:** OTLP: invalid UTF-8 in a log body is replaced with U+FFFD without notice

**Repository:** grafana/loki
**Version:** 3.1.1 (`grafana/loki:3.1.1`), default single-binary config
**Reproduced first-hand:** yes, on 2026-09-08.

## What happens

An OTLP/HTTP JSON export whose log body contains the bytes `0xff 0xfe` is
accepted with `204 No Content`. The stored line is:

```
before-\xef\xbf\xbd\xef\xbf\xbd-after
```

Each invalid byte has been replaced with U+FFFD. Nothing errors and nothing
warns, and the line read back is not the line that was sent.

## Reproduction

The payload has to contain raw invalid bytes, so it is written with `printf`
rather than an editor.

```sh
docker run -d --name loki -p 3100:3100 grafana/loki:3.1.1
until curl -sf localhost:3100/ready >/dev/null; do sleep 2; done

NS=$(( $(date +%s) * 1000000000 ))
printf '%s' '{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"utf8demo"}}]},"scopeLogs":[{"scope":{"name":"demo"},"logRecords":[{"timeUnixNano":"'$NS'","observedTimeUnixNano":"'$NS'","severityNumber":9,"severityText":"INFO","body":{"stringValue":"before-' > /tmp/utf8.json
printf '\xff\xfe' >> /tmp/utf8.json
printf '%s' '-after"}}]}]}]}' >> /tmp/utf8.json

curl -s -o /dev/null -w 'ingest HTTP %{http_code}\n' -X POST localhost:3100/otlp/v1/logs \
  -H 'Content-Type: application/json' --data-binary @/tmp/utf8.json
sleep 3
curl -sG localhost:3100/loki/api/v1/query_range \
  --data-urlencode 'query={service_name="utf8demo"}' \
  --data-urlencode "start=$((NS - 600000000000))" --data-urlencode "end=$((NS + 600000000000))" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["data"]["result"]; print(repr(r[0]["values"][0][1]) if r else "no rows")'
```

Observed: `ingest HTTP 204`, and the stored line carries two U+FFFD where the
two bytes were.

## Why this is being asked rather than asserted

Protobuf string fields must be valid UTF-8 and a decoder is permitted to reject
a payload that is not — two other stores refuse this one with a `400`, which is
a perfectly good answer. Replacement is also a reasonable choice, **when it is
documented**, because then an operator can know that a U+FFFD in a line may be
Loki's rather than the application's.

So the question is whether this substitution is documented somewhere we have
missed. If it is, this is not a defect and we would like to link the
documentation from the check. If it is not, a note in the OTLP ingestion
documentation would be enough: the behaviour is defensible and the silence is
the problem.

A per-entry indication that a substitution occurred would be better still, but
that is a larger request than this report is making.

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The check is
`cases/otlp-logs/body-invalid-utf8.yaml`, whose rule reads: rejecting is a
REJECT, accepting and replacing the bytes is an ALTER unless the backend
documents the replacement.
