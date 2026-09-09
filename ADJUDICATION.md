# Adjudication

`AGENTS.md`'s "Adjudication" section gives the order of precedence a check's
verdict rests on:

1. **The specification says so.** Cite the clause. Done — a check written
   this way never needs what follows.
2. **The specification is silent but a reference implementation is the de
   facto definition.** Mark the check `basis: de-facto`, name the reference,
   and it is settled the same way.
3. **Genuinely ambiguous** — the specification is silent and there is no
   reference implementation to defer to. This is the only case that reaches
   the rest of this document.

A check in the third case ships as `match: present`: it records what each
store did without judging it, because there is no rule yet to judge it
against. This file is how one gets a rule, so the check can be promoted to
`match: exact`.

## The process

1. **The disagreement is real, not a harness bug.** Before anything else,
   rule out the alternative `CLAUDE.md` names: a poll shorter than a store's
   commit interval, a read-back selector that assumed a label, a case
   measured before a store finished starting. If ruling it out is not
   possible from the case's own run, it is not ready for adjudication.
2. **Open an issue from the `Adjudication` template**
   (`.github/ISSUE_TEMPLATE/adjudication.md`), naming the check, what every
   implementation that has run it did, and precisely what the specification
   does not settle — quoting the clause and saying where it stops, not
   paraphrasing it.
3. **Put the question to the specification's owners** — the standards body,
   or the project that maintains the de facto definition when there is no
   formal spec — and link that discussion from the issue. An issue with no
   upstream question linked stays open with nothing to promote: recording a
   divergence is the point, and guessing at what the specification's authors
   would say is exactly what this process exists to avoid.
4. **Record the answer verbatim in the issue and in the check's `notes:`**
   when it arrives, with the date and who gave it.
5. **Promote the check only once a rule exists.** Change `match: present` to
   `match: exact`, cite the answer in `rule:`, and re-run the suite: every
   implementation that disagreed with the new rule is now `ALTER`, not
   `present`, and that is the adjudication doing its job — a `present` check
   never fails a backend for the divergence it records; an `exact` one does,
   because there is now a rule it violates.

A check does not sit at `present` forever by default. Every one gets an
adjudication issue, even if the issue stays open for years waiting on an
answer — `AGENTS.md`'s "Never" section is explicit that a `present` case is
not simply left as the final answer.

## Two implementations disagreeing is not, on its own, a rule

The order of precedence above is deliberately not "whichever behaviour is
more common" or "whichever behaviour the newest implementation chose." A
specification that leaves something unsettled leaves it unsettled regardless
of how many stores converge on one answer by accident; convergence is
evidence worth citing in the issue, not a substitute for an answer from the
people who own the specification.

## Worked example

`cases/otlp-logs/timestamp-nanosecond-precision.yaml` is the first case this
process was written for. Five stores answered five different ways for how
much of `time_unix_nano`'s precision survives a round trip — millisecond
string, microsecond integer, a nanosecond integer that turned out to hold
only whole microseconds, and two that kept every digit. The OpenTelemetry
logs specification defines what the field means and says nothing about what a
receiver must retain, so no rule exists yet to promote the check with, no
matter how the five stores split. The case's own `notes:` record each
measurement as it was made; see there for the reproductions.
