# Design

## The shape of the problem

A backend can mishandle data at four points. They differ in how loudly they fail,
and the quiet ones are the ones worth building a tool for.

1. **Ingest refuses.** The write returns an error. Loud, found immediately.
2. **Ingest accepts and discards.** The write succeeds, the data never lands.
3. **Ingest accepts and changes.** The data lands, altered.
4. **Query disagrees.** The data is intact, but the same query returns different
   rows than another backend returns for the same corpus.

Testing only ingest catches 1 and part of 2. Catching 3 and 4 requires reading
the data back and comparing, which is why every check is a round trip.

## A check

A check is a declarative file. It names the rule it tests, the payload to send,
and what a conformant backend must return when asked for it again.

```yaml
id: otlp-logs/schema-url-omitted
protocol: otlp-logs
title: A resource with no schemaUrl is accepted
rule:
  spec: opentelemetry-proto/logs/v1
  text: >
    schema_url is an optional field. Proto3 JSON encoding omits fields set to
    their default value, so a Collector emits no schemaUrl when it has none.
send:
  format: otlp-json
  body: cases/otlp-logs/schema-url-omitted.json
expect:
  ingest: accepted
  readback:
    match: exact
    on: [body, severityText, timeUnixNano]
```

`readback.on` lists the fields that must survive. Anything the backend is
permitted to add — its own ingest timestamp, an internal document id — is
excluded rather than compared, so the check does not fail for reasons that are
not divergences.

## The runner

```
      cases/                     backends/<name>.yaml
        │                              │
        ▼                              ▼
   load corpus  ──────►  adapter: send / read back / normalise
                                       │
                                       ▼
                              PASS | REJECT | ALTER
                                       │
                                       ▼
                        report: table, JSON, or matrix page
```

Each backend adapter supplies four things and nothing more:

| Adapter provides | Example |
| --- | --- |
| Ingest endpoint and headers | `POST /v1/logs`, `X-P-Stream: <index>` |
| Auth | basic, bearer, none |
| Read-back query | how to ask "give me the record I just sent" |
| Normalisation | strip fields the backend legitimately adds |

Everything else is shared. Adding a backend should be a configuration file, not
a code change, or the roster will never grow past the ones the author uses.

## Finding the record again

Two things make read-back harder than it looks, and both have to be settled
before the runner is written.

**Every record carries a run key.** The runner stamps each payload with a
unique attribute (`specmatrix.run`, a random id per check per run) and the
read-back query filters on it. Without this, "give me the record I just sent"
means "give me everything in a time window", which breaks the moment two checks
share a stream or a rerun leaves state behind. The key is added to the payload
by the runner, not written into the case file, so a maintainer can still send
the raw file with `curl`.

**Ingest is asynchronous.** Most backends acknowledge a write before it is
queryable. Read-back therefore polls: query, and if the record is absent, wait
and retry up to a per-backend timeout. Only after the timeout does absence mean
`ALTER`. A runner that reads back once, immediately, will report every backend
as dropping data.

## Deciding a verdict

```
send ──► ingest error?  ── yes ──► REJECT
           │ no
           ▼
      read back ──► record absent?  ── yes ──► ALTER (accepted then dropped)
           │ present
           ▼
   compare declared fields ── differ ──► ALTER
           │ equal
           ▼
          PASS
```

Accepting a write and then losing the record is classified `ALTER` rather than a
separate verdict: from the caller's side, silently discarding data and silently
changing it are the same failure, and both look like success at write time.

## Normalisation, and why it is dangerous

Every backend adds fields of its own. Comparing raw documents would make every
check fail everywhere and the tool would be useless.

The temptation is to normalise until things match. That is how a conformance
suite becomes worthless: normalise hard enough and everything passes.

The rule here is that **normalisation is declared per backend and reviewed as
part of adding that backend**. A backend adapter may strip fields it adds. It may
not rewrite values, coerce types, or reorder anything a specification says is
ordered. When a maintainer wants a normalisation that changes a value, that is
not a normalisation — it is a divergence, and it belongs in the results.

## Adjudication

Two backends will disagree and both will claim to be right. Deciding which is
wrong is the part that gives the project authority, and it cannot be automated.

The order of precedence:

1. **The specification says so.** Cite the clause in the check. Done.
2. **The specification is silent, but a reference implementation is the de facto
   definition.** For an Elasticsearch-compatible API, Elasticsearch decides. Say
   so explicitly in the check, and mark the check as `de-facto` rather than
   `spec` so readers can weigh it differently.
3. **Genuinely ambiguous.** Do not ship a verdict. Record the divergence, report
   it upstream, and let the ambiguity be resolved by the people who own the
   specification. A check that encodes a guess will eventually be used to
   embarrass someone unfairly, and the project's credibility does not survive
   that.

Checks carry their basis so a reader can tell the difference:

```yaml
rule:
  basis: spec        # or: de-facto
  spec: prometheus/remote-write/1.0
  section: stale-markers
```

## Non-goals

**Benchmarking.** Throughput and latency depend on hardware and tuning. Mixing
performance numbers into a correctness matrix invites vendors to argue about the
benchmark instead of the divergence.

**Scoring or ranking.** No single number. A backend that fails ten cosmetic
checks is not worse than one that silently drops metrics. Publish the cases and
let readers weigh what matters to them.

**Feature comparison.** Whether a backend supports a feature at all is a
different question from whether its implementation behaves as specified. Feature
tables already exist and are usually maintained by someone with an interest.
