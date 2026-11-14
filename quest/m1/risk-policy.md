# [S] Declare which changes are high risk

## Goal

A file checked into this repository tells `quest` how risky a change here
is to merge, so each PR's guardrails match its blast radius:

- The paths and surfaces that are always high: whatever the
  [questline](/quest/README.md) calls breaking (the public Rust, JS, and
  bindings API, a frozen wire version, the hang catalog JSON, stored and
  exported container formats, CLI flags, config keys, and environment
  variables), plus workflow files, release tooling, and this file itself.
- The full suite a high-tier PR runs in the queue, `just gate`, where
  every other PR keeps the checks its diff selects.
- The classes of change that land without the maintainer's pick, starting
  with tests, benchmarks, and internal docs, each added or removed only by
  a PR the maintainer picks.

`CONTRIBUTING.md`'s landing gate and `REVIEW.md` point at the file instead
of restating it, and a PR that changes none of this lands as it does
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

Agent's choices, for the implementer to revisit:

- Write the always-high list from the questline's definition of breaking
  and the Impact list `CONTRIBUTING.md` already requires, so the three
  cannot drift: one of them becomes the source the others cite.
- Say in the PR what `just gate` costs in the queue against the scoped
  median of 5.5 minutes, and how often a PR would be high, from the last
  month's merges.
- The file's format is `quest`'s; take it from the risk tier's quest there
  and add nothing local.

## Waiting

- moq-dev/quest: its quests `quest/m0/benchmark/risk-tier.md` and
  `quest/m0/benchmark/guardrails.md` land and this repository's pinned
  `quest` carries them; check both are gone from its tree and
  `quest --version` here

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the scoped gate the low
  and medium tiers keep
- [Cost audit](/quest/m3/audit-cost.md) - weighs each check's cost, and can
  read what the full suite caught on high-tier PRs
