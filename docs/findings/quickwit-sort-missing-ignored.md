**Title:** Elasticsearch API: the `missing` sort option is ignored

**Repository:** quickwit-oss/quickwit
**Version:** 0.8.2 (`quickwit/quickwit:0.8.2`)
**Reproduced first-hand:** yes, on 2026-09-08, against Elasticsearch 8.15.0 as the reference.

## What happens

Sorting ascending on a field that some documents lack returns the same order
whether `"missing": "_first"` is given or not. The option has no effect and no
error is raised.

Elasticsearch places documents lacking the sort field last by default and first
when asked. Quickwit agrees with the default and does not implement the option.

## Reproduction

```sh
docker run -d --name qw -p 7280:7280 quickwit/quickwit:0.8.2 run
until curl -sf localhost:7280/health/livez >/dev/null; do sleep 1; done

curl -s -X POST localhost:7280/api/v1/indexes -H 'Content-Type: application/json' \
  -d '{"version":"0.8","index_id":"sortdemo","doc_mapping":{"mode":"dynamic"}}'

printf '%s\n' \
 '{"index":{"_index":"sortdemo"}}' '{"doc":"lower","rank":2}' \
 '{"index":{"_index":"sortdemo"}}' '{"doc":"mixed","rank":1}' \
 '{"index":{"_index":"sortdemo"}}' '{"doc":"norank"}' \
 | curl -s -X POST localhost:7280/api/v1/_elastic/sortdemo/_bulk \
     -H 'Content-Type: application/x-ndjson' --data-binary @-

# A newly created index defaults to commit_timeout_secs: 60.
sleep 70

for opt in '{"rank":{"order":"asc"}}' '{"rank":{"order":"asc","missing":"_first"}}'; do
  curl -s -X POST localhost:7280/api/v1/_elastic/sortdemo/_search \
    -H 'Content-Type: application/json' -d "{\"query\":{\"match_all\":{}},\"sort\":[$opt]}" \
    | python3 -c 'import json,sys; print([h["_source"]["doc"] for h in json.load(sys.stdin)["hits"]["hits"]])'
done
```

Observed on Quickwit — both lines identical:

```
['mixed', 'lower', 'norank']
['mixed', 'lower', 'norank']
```

Elasticsearch 8.15.0, same documents and queries:

```
['mixed', 'lower', 'norank']
['norank', 'mixed', 'lower']
```

## Why this is worth reporting

There is no Elasticsearch specification, so its behaviour is the definition of
the API Quickwit offers compatibility with. Where the sort places documents that
lack the field decides which rows the first page of a paged query returns, so a
dashboard built on such a sort shows different rows against the two stores, with
nothing to indicate why.

Rejecting the option would also resolve it: an error tells the caller the query
does not mean what it says, which silence does not.

## Where this came from

SpecMatrix, a conformance corpus for observability backends. The checks are
`cases/es-bulk/sort-missing-first.yaml` and `cases/es-bulk/sort-missing-last.yaml`,
which are a pair on purpose — the second passes on Quickwit, and only having
both shows the disagreement is the option rather than the ordering.
