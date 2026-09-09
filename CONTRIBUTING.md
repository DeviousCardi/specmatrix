# Contributing

Two kinds of contribution matter most: a check that cites a real rule, and a
backend adapter someone will keep running.

## Adding a check

A check needs a reason to exist. Pull requests adding checks without one will be
asked for the citation rather than merged and tidied later, because the corpus's
value is entirely in every entry being defensible.

1. Write `cases/<protocol>/<id>.yaml` with a `rule` block naming the clause, the
   reference implementation, or the filed bug it comes from.
2. Put the payload beside it in its native format, so it can be sent by hand.
3. State `basis: spec` or `basis: de-facto`. If you cannot pick one, the check is
   not ready.
4. Run it against at least two backends. A check that passes everywhere is still
   worth having; a check that fails everywhere usually means the check is wrong.

### If two backends disagree and you are unsure who is right

Say so in the pull request and leave the verdict out — ship the check as
`match: present`. Then open an issue from the `Adjudication` template and work
through [`ADJUDICATION.md`](ADJUDICATION.md#the-process): the question goes to
whoever owns the specification, and the check is promoted to `match: exact`
only once they answer.

A wrong verdict published under this project's name damages someone's reputation
unfairly and destroys the project's own. Being slow is much cheaper than being
confidently wrong.

## Adding a backend

Work through the procedure in [`AGENTS.md`](AGENTS.md#adding-a-backend). The
parts that get skipped, in order of how often:

- **Unattended startup.** If it needs manual setup it will not run in CI, and a
  column that stops running becomes a stale claim.
- **Teardown.** Without it the second run tests different state than the first.
- **Version recording.** A result without a version is not reproducible.

### Normalisation

An adapter may strip fields the backend adds. It may not rewrite a value, coerce
a type, or reorder anything the specification orders.

If a check only passes once you normalise a value, you have found a divergence
and started to hide it. That is the failure mode that turns a conformance suite
into marketing, and reviewers will push back on it specifically.

## Reporting a divergence upstream

Findings are worth more filed than tabulated.

- File in the backend's own tracker, not here.
- Include the payload file and the exact request, so it reproduces without the
  tool installed.
- Say what the specification requires and quote it.
- Say plainly whether you reproduced it or are relaying a report.
- Link the check, so the maintainer can see it will be regression-tested.

Then add the issue link to the check. A check that carries the bug it caught is
how a reader judges whether this project is worth their attention.

## Tone in results

The matrix will show projects failing. Some of those projects are small, and some
maintainers will read it as an attack.

Write findings the way you would want one written about your own code: state the
behaviour, cite the rule, do not editorialise, and give the maintainer the
reproduction. No scores, no rankings, no "worst backend" framing.

## Conduct

Be straightforward and assume competence. Most divergences are a reasonable
reading of an ambiguous specification rather than carelessness, and the write-up
should reflect that.
