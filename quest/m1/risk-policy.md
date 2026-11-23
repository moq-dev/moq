# [XS] Name the high tier's full suite and the classes that land on their own

## Goal

[RISK.md](/RISK.md) also tells `quest` what each tier's guardrails need
here:

- The full suite a high-tier PR runs in the queue, `just gate`, where
  every other PR keeps the checks its diff selects.
- The classes of change that land without the maintainer's pick, starting
  with tests, benchmarks, and internal docs, each added or removed only by
  a PR the maintainer picks.

`CONTRIBUTING.md`'s landing gate points at the file for both instead of
restating them, and a PR whose tier acts on neither lands as it does
today.

## Plan

Decided 2026-11-13 (maintainer, `/quest-plan` in moq-dev/quest;
[moq-dev/quest#711](https://github.com/moq-dev/quest/pull/711) holds each
decision with its rejected options): every PR gets a risk tier, low,
medium, or high, set from its diff, which scales its review, its CI, and
whether it needs the maintainer's pick. Low-tier merging without a pick is
earned one class of change at a time, and only the maintainer widens or
narrows a class.

- ✅ High-tier PRs run the full suite at the gate (rejected: scoped checks
  at every tier). This amends [CI cost and latency](/quest/m0/ci/README.md)'s
  "builds and tests run only what the diff can affect" for the high tier
  only. The landing gate already says scoped checks mean "a break can
  reach `main`".
- ✅ m1 (maintainer; rejected: m0, m4). The questline's rule would put
  tooling in m4; the maintainer placed it with this cycle's work.

`RISK.md` already sets the tiers: its `## High` paths and the prose the
classifier reads, with [Releases](/RELEASE.md#releases) as the one
definition of an API break and a wire change. `quest`'s format has no
place yet for a full-suite command or a class list; its guardrails quest
adds both, so this quest only fills them in.

Agent's choices, for the implementer to revisit:

- Say in the PR what `just gate` costs in the queue against the scoped
  median of 5.5 minutes, and how often a PR would be high, from the last
  month's merges. The Nightly Gate job took 10m51s on 2026-11-18, and
  `RISK.md`'s paths, with `quest`'s own, held 166 of 2,322 merges from
  2026-10-19 high, before the classifier's share.
- The file's format is `quest`'s; take it from the guardrails quest there
  and add nothing local.

## Waiting

- moq-dev/quest: its quest `quest/m0/benchmark/guardrails.md` lands and
  this repository's pinned `quest` carries it; check it is gone from its
  tree and `quest review --help` here names a full-suite command and
  classes

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the scoped gate the low
  and medium tiers keep
- [Cost audit](/quest/m3/audit-cost.md) - weighs each check's cost, and can
  read what the full suite caught on high-tier PRs
