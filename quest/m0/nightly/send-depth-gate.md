# [S] The merge gate checks the session driver's Send depth

## Goal

A PR that pushes the session driver's `Send` proof past the compiler's default
recursion limit fails the merge gate, instead of landing and breaking every
Fuzz job in the next Nightly. The check is the one Nightly already runs
(`just rs send-depth` on the pinned nightly toolchain, and the Fuzz build's
`-D recursion_depth_exceeding_limit`).

## Plan

Found by the triage of Nightly run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(2026-11-13): every Fuzz job failed with `overflow evaluating the requirement
driver::Driver: Send`. [moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)
deepened the proof past the limit, and
[moq-dev/moq#8657](https://github.com/moq-dev/moq/pull/8657) fixed it with 12
levels to spare, so the next addition to session state can break it again.

Decided 2026-11-13 (maintainer, /quest-plan): ✅ add it to the merge gate
(rejected: keep it Nightly-only, which lets the break land on `main`; drop
the quest). Measure what it adds to the gate and say so in the PR, per the
gate-cost rule. Reuse the existing recipe; pick the cheapest gate job that
already has the pinned nightly toolchain.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: [moq-dev/moq#8657](https://github.com/moq-dev/moq/pull/8657) merges, so the gate starts from a passing proof; check with `gh pr view 8657 --json state`
