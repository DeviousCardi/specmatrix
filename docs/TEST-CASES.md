# The corpus

## The rule that keeps this honest

**Never add a check you have not seen matter.**

Every check must come from one of three places:

1. A clause in a specification that says MUST, MUST NOT, or optional.
2. A divergence observed between two implementations.
3. A filed bug, cited.

Checks invented to pad the matrix make it look thorough and make it worthless.
They fail for reasons nobody cares about, maintainers learn to ignore the
results, and the project becomes a toy. If a check cannot cite why it exists, it
does not go in.

## Seed cases

These come from bugs in real projects. The status column is deliberate: some were
reproduced first-hand, others are reported and not yet confirmed here, and the
difference matters when a maintainer reads the results.

| Case | Protocol | Origin | Status |
| --- | --- | --- | --- |
| NaN in histogram `_count` (stale marker) | remote-write | [vectordotdev/vector#26228](https://github.com/vectordotdev/vector/issues/26228) | **Reproduced.** Whole write request rejected with 400; every unrelated sample in the batch lost with it |
| NaN `_count` beside a finite `_sum` and buckets | remote-write | Found while fixing the above | **Reproduced.** Accepted, then emitted with a fabricated `count: 0` — an `ALTER` |
| `schemaUrl` omitted from OTLP JSON | OTLP logs, metrics | [parseablehq/parseable#1751](https://github.com/parseablehq/parseable/issues/1751) | **Not reproducible** on the reported version. Kept because proto3 omits empty strings, so a conformant Collector really does send this |
| Non-UTF-8 syslog message body | syslog | [vectordotdev/vector#20462](https://github.com/vectordotdev/vector/issues/20462) | Reported, not confirmed here |
| `must_not` / leading `-` matches records where the field is absent | ES query | [quickwit-oss/quickwit#6474](https://github.com/quickwit-oss/quickwit/issues/6474) | Reported, not confirmed here |
| `field:*` returns nothing when field-presence indexing is off | ES query | [quickwit-oss/quickwit#6475](https://github.com/quickwit-oss/quickwit/issues/6475) | Reported, not confirmed here |

Two of these are `ALTER` cases — accepted, then wrong — which is the class the
tool exists for and the class nobody currently tests.

## Categories to build out

Each category below is a family of checks per protocol. The count is a target,
not a plan; write the ones that cite something.

### Optional and absent fields

Proto3 JSON omits fields holding default values, so anything optional is
routinely absent from real traffic. Implementations that parse with a required
field fail on conformant input.

Check every optional field for: omitted, present-and-empty, explicitly null.

### Absent-field query semantics

The largest source of silent divergence, and the one that changes results rather
than erroring.

- Negation over a field some records lack
- `exists` and wildcard-existence queries
- Sorting when the sort field is missing
- Aggregating a field present in only some records

SQL, Elasticsearch and Lucene each answer these differently. A backend claiming
Elasticsearch compatibility has to match Elasticsearch, not SQL intuition.

### Special float values

`NaN`, `+Inf`, `-Inf`, `-0.0`. Remote-write **requires** NaN as a stale marker,
so rejecting NaN is not defensive — it is non-conformant. Check them in counts,
sums, bucket bounds, quantiles, and gauges.

### Encoding

Invalid UTF-8, lone surrogates, embedded NUL, control characters, very long
single tokens, mixed encodings within one batch. Replacement is acceptable if
documented; silent truncation is an `ALTER`.

### Empty and boundary collections

Empty batch, resource with no attributes, histogram with no buckets, zero-length
body, a batch containing exactly one record, a batch at the size limit.

### Numeric limits

`u64::MAX`, `i64::MIN`, floats that round-trip only at full precision, integers
beyond IEEE-754 exact range, negative counts where the spec forbids them.

### Timestamps

Zero, negative, far future, far past, nanosecond precision preserved or
truncated, and timestamps arriving out of order within a batch.

### Duplicates and ordering

Duplicate attribute keys in one record, duplicate label names, repeated series in
one write, and whether ordering the specification guarantees is preserved.

## File layout

```
cases/
  otlp-logs/
    schema-url-omitted.yaml
    schema-url-omitted.json
    body-invalid-utf8.yaml
    body-invalid-utf8.json
  remote-write/
    histogram-nan-count.yaml
  es-query/
    must-not-absent-field.yaml
```

The `.yaml` declares the check; the payload sits beside it in its native format.
Keeping payloads as real OTLP JSON or real protobuf rather than generating them
means a maintainer can `curl` the file straight at their own backend to confirm a
finding, which is the difference between a bug report they act on and one they
have to reproduce themselves first.
