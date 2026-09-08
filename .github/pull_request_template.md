## What this changes

<!-- One or two sentences. -->

## If this adds or changes a check

- [ ] `rule.basis` is `spec` or `de-facto` — if you cannot pick one, the check is not ready
- [ ] `rule.text` quotes the clause, names the reference implementation, or links the filed bug
- [ ] Run against **at least two** backends, with both verdicts recorded verbatim in `notes:` with versions
- [ ] The payload is in its native format beside the case, so it can be sent with `curl`

A check that passes everywhere is still worth having. A check that fails
everywhere usually means the check is wrong.

## If this adds or changes an adapter

- [ ] Every endpoint, header and field pointer was **confirmed by hand against a running container**, not taken from documentation
- [ ] The image tag is pinned — never `latest`
- [ ] The header comment says what was confirmed and on what date
- [ ] `normalise` lists only fields the backend *adds*; no value is rewritten, no type coerced, nothing reordered
- [ ] The control passes, and the suite gives the same verdicts twice in a row

If a check only passes once you normalise a value, you have found a divergence
and started to hide it.

## If this reports a divergence

- [ ] Filed in the backend's own tracker first, and the issue is linked from the check's `rule.observed`
- [ ] The report reproduces without this tool installed
- [ ] It says plainly whether you reproduced it or are relaying a report

## Always

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
- [ ] No score, percentage, badge or ranking anywhere
