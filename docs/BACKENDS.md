# Backends

## How many to integrate

Not as many as possible. A matrix with twenty columns and eight rows says less
than one with six columns and eighty rows, and it costs far more to keep honest:
every backend has to be kept running, upgraded, and re-tested, or its column
silently becomes a claim about a version nobody uses any more.

The roster grows when a backend earns a column:

- it claims a protocol the corpus covers, and
- it runs unattended in CI from a single container, and
- someone will keep it upgraded.

The third condition is the one that gets skipped and the one that rots matrices.

### Phasing

| Phase | Protocols | Backends | Checks | Purpose |
| --- | --- | --- | --- | --- |
| 0.1 | OTLP/HTTP logs (JSON) | 2 | 3 | Prove the round trip works and find the first real divergence |
| 0.2 | + Elasticsearch `_bulk` / `_search` | 6 | ~25 | First publishable matrix |
| 0.3 | + Prometheus remote-write | 9 | ~45 | Covers metrics, where silent alteration is worst |
| 1.0 | + Loki push, OTLP traces | 12 | ~80 | Enough to be cited |

Ship 0.1 to yourself, not to the internet. The first public artefact should be
0.2: three protocols is a framework, six backends with real findings is a story.

## Protocols and their reference

Adjudication needs a source of truth per protocol. Some have a specification;
some only have an implementation everyone imitates.

| Protocol | Basis | Reference |
| --- | --- | --- |
| OTLP/HTTP, OTLP/gRPC | Specification | `opentelemetry-proto` + OTLP spec |
| Prometheus remote-write | Specification | remote-write 1.0 / 2.0 spec |
| Elasticsearch API | De facto | Elasticsearch itself |
| Loki push API | De facto | Loki itself |
| Splunk HEC | De facto | Splunk |

Where the basis is *de facto*, the reference implementation is a column in the
matrix like any other, and its behaviour defines conformance for that protocol.
That asymmetry should be stated plainly in the results rather than hidden.

## Candidate roster

Endpoint paths below are starting points for the adapter, not verified contracts,
except where marked. Confirm each against the running container when writing its
adapter.

### OTLP receivers that store and can be queried

| Backend | Container | Ingest | Read back | Notes |
| --- | --- | --- | --- | --- |
| Parseable | `parseablehq/parseable` | `POST /v1/logs`, `/v1/metrics` **(verified)** | SQL query API | Needs `X-P-Stream` and `X-P-Log-Source` headers; basic auth **(verified)** |
| Quickwit | `quickwit/quickwit` | `POST /api/v1/<index>/ingest` | `/api/v1/<index>/search`, `_elastic/<index>/_search` **(verified)** | Single binary, no deps |
| OpenObserve | `openobserve/openobserve` | OTLP HTTP | SQL + ES-compatible search | Also a remote-write receiver |
| SigNoz | compose stack | OTLP | ClickHouse-backed query API | Heavier; multi-container |
| VictoriaLogs | `victoriametrics/victoria-logs` | OTLP, Loki, Elasticsearch bulk | LogsQL | Unusually broad protocol surface |
| Elasticsearch | `elasticsearch` | OTLP endpoint, `_bulk` | `_search` | Reference for the ES protocol |
| Grafana Loki | `grafana/loki` | OTLP, push API | LogQL | Reference for the Loki protocol |

### Prometheus remote-write receivers

| Backend | Container | Notes |
| --- | --- | --- |
| Prometheus | `prom/prometheus` | Reference. Needs `--web.enable-remote-write-receiver` |
| VictoriaMetrics | `victoriametrics/victoria-metrics` | Widely deployed |
| Mimir | `grafana/mimir` | Multi-tenant; needs a tenant header |
| Thanos Receive | `thanosio/thanos` | |
| GreptimeDB | `greptime/greptimedb` | Also OTLP |
| Vector | `timberio/vector` | A pipeline rather than a store; see below |
| OpenObserve | `openobserve/openobserve` | |

### Pipelines rather than stores

Vector, the OpenTelemetry Collector, Fluent Bit and Alloy receive these protocols
but do not store anything, so there is nothing to query back.

They are still worth a column, with a different read-back: configure a file sink,
then read the file. That tests ingest handling and any transformation the
pipeline applies, which is where they can silently alter data — and it is exactly
where the corpus's seed cases came from.

Treat this as a separate suite. Mixing pipelines and stores in one table implies
a comparison that does not hold.

## Adapter checklist

An adapter is done when:

- [ ] Container starts unattended with no manual setup
- [ ] Index or stream is created by the adapter, not by hand
- [ ] Ingest returns a status the runner can classify
- [ ] Read-back returns a single record by a key the check controls
- [ ] Normalisation lists only fields the backend adds, never a rewritten value
- [ ] Teardown removes state, so a rerun starts clean
- [ ] Version is recorded in the results

The last one matters more than it looks. A matrix without versions is a claim
about the past that reads as a claim about the present.
