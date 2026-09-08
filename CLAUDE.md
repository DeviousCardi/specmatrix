# CLAUDE.md

Read [`AGENTS.md`](AGENTS.md) first. It is the working guide for this
repository — the design that constrains changes, and how to add a check, an
adapter, or a finding. Everything there applies here.

## Notes specific to working in this repository

**Confirm against a running container, never from memory.** Endpoints, headers,
field pointers, response shapes. Plausible-sounding API details are the single
most common way to put a false claim into an adapter, and an adapter is a
published claim about someone else's software.

**A surprising result is your bug until proven otherwise.** Every "finding" this
project nearly published by mistake was the harness: a poll shorter than a
store's commit interval, a query settling on two empty reads, a read-back
selector that assumed a label, a create issued while a delete was in flight, a
case measured before a store had finished starting. Before recording a
divergence, produce the control that rules out the alternative — the same
payload in a shape that works, at the same moment, against the same store.

**Record what happened, not what you expected.** Verdicts go into `notes:`
verbatim, with versions and dates. If a result contradicts a note already there,
say so and investigate rather than smoothing it over.

**Do not run destructive Docker commands beyond this project's own containers.**
`specmatrix-*` is the naming; other containers and volumes on the machine belong
to something else.

**Planning documents are not in this repository.** Do not reconstruct a roadmap
here, and do not add design documents — the design lives in `AGENTS.md` and the
reasoning behind each check lives in the check.
