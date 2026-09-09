# Governance

The results are only worth reading if they are not for sale. This is written
down before anyone has a reason to ask.

## Maintainers

The project currently has one maintainer, [@DeviousCardi](https://github.com/DeviousCardi),
who has commit access and decides what merges. That is a starting point, not
a target: a project with one maintainer has a bus factor of one, and this file
exists so growing past that happens by a stated process rather than by
whoever shows up first getting write access.

### How a maintainer is added

1. Several substantive pull requests merged — a check with a real citation, an
   adapter re-confirmed by hand, or a runner fix, not typo corrections.
2. A public proposal, as an issue on this repository, naming the person and
   what they have contributed.
3. No standing maintainer objects within two weeks.

Removal follows the same shape: a public issue, a reason, two weeks for
objection. A maintainer who has been inactive for two consecutive quarterly
reruns (`docs/BACKENDS.md`'s checklist, run by `.github/workflows/matrix.yml`)
is asked directly before being proposed for removal, not removed by default —
absence is not the same as disagreement.

## Vendor conflict of interest

A maintainer employed by, or paid by, a vendor whose backend appears in the
matrix **declares it in this file**, in the table below. A maintainer with a
declared interest in a vendor:

- Does not adjudicate a case (`ADJUDICATION.md`) where that vendor's backend
  is the reference implementation or the backend under dispute.
- Does not merge a pull request that changes that vendor's adapter, files a
  finding against it, or resolves a `divergence` issue about it — review and
  approval come from another maintainer.
- May still write cases, fix the runner, and do everything else a maintainer
  does; the recusal is scoped to decisions about the vendor they are
  affiliated with, not a blanket exclusion.

An affiliation is declared before the maintainer takes any of the actions
above, not after being asked about it.

| Maintainer | Vendor | Relationship |
| --- | --- | --- |
| _(none declared)_ | | |

## Funding

None. If that changes, the amount and source are added to
[`README.md`](README.md#funding), not only here — a reader deciding whether
to trust a result should not have to find this file to learn who paid for it.

## Scope of this document

This covers who decides what merges and who may declare a vendor conflict of
interest away from a decision. It does not cover the rule for what a check
may assert, which is `AGENTS.md`'s "Adjudication" section and made
operational in [`ADJUDICATION.md`](ADJUDICATION.md); it does not cover how a
backend maintainer disputes a specific published verdict, which is
`.github/ISSUE_TEMPLATE/divergence.md`.
