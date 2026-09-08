# Security

## Reporting a vulnerability

Report privately through [GitHub Security
Advisories](https://github.com/DeviousCardi/specmatrix/security/advisories/new).
Please do not open a public issue for a vulnerability.

## What this tool does on your machine

Worth knowing before you run it, because it does more than read files:

- **It sends payloads to whatever URL you point it at**, some of them
  deliberately malformed — invalid UTF-8, embedded NULs, hundred-thousand
  character tokens, integers at the edge of the range. That is the point of the
  corpus. Point it at a store you are willing to have written to.
- **It writes to that store**, and with `teardown` declared it *deletes the
  stream or index a case uses* before each case. Every adapter in this
  repository scopes that to a `specmatrix_`-prefixed name derived from the case
  id, but read the adapter before running it against anything you care about.
- **`--manage` runs `docker`** to start and remove containers named
  `specmatrix-<backend>`, and removes any existing container of that name first.
- **It does not phone home.** There is no telemetry, no upload, and no network
  access beyond the backend URL you give it and the container images Docker
  pulls.

## Scope

A vulnerability here means something like: a case payload that can escape the
runner, an adapter field that can execute or inject beyond the request it
describes, a rendered page that executes content it was given, or a dependency
advisory that reaches this code.

A backend behaving badly is **not** a vulnerability in this project — it is a
finding, and it goes through
[`CONTRIBUTING.md`](CONTRIBUTING.md) to that backend's own tracker.

## What is enforced here

- Pull requests only on `main`; no direct pushes, no force pushes, no deletion
- Review required from a code owner, and CI green, before merge
- GitHub Actions run with a read-only token and are pinned to commit SHAs, so a
  moved tag cannot change what runs
- `cargo audit` on every pull request
- Dependabot on both Cargo and Actions, weekly
- Secret scanning with push protection

## A note on the corpus

`cases/` contains payloads that are malformed on purpose, including bytes that
are not valid UTF-8. Some scanners flag them. They are fixtures, they are never
executed, and each one is described by the check beside it.
