# [S] Model resume skips and failover source closure in TLA+ specs

## Goal

Bring `ResumeReader.tla` and `FailoverChain.tla` into compliance with the model
coverage rule before advancing their sync markers:

1. `ResumeReader.tla` models publisher-declared skip evidence across resume
   copies so TLC verifies that a viewer stops waiting at clean end when one
   copy proves a missing group was skipped.
2. `FailoverChain.tla` models source closure before copy failure as a separate
   transition from route withdrawal when an upstream session dies while its
   route is still advertised.

## Plan

Found 2026-11-27 by replaying `gpt-6.2-sol` (`medium`) on
[moq-dev/moq#9627](https://github.com/moq-dev/moq/pull/9627), which merged
unreviewed under the `low` tier.

- Caused by: [moq-dev/moq#9627](https://github.com/moq-dev/moq/pull/9627)
  (list under `## Caused by` in the fixing PR).
- Resume model (`.claude/skills/verify-resume/SKILL.md` / `ResumeReader.tla`):
  when a viewer switches relays and one copy proves the publisher skipped a
  missing group, the implementation excludes the group before checking whether
  another copy can deliver it. Add skip state across copies to
  `ResumeReader.tla` so `CleanEndIsComplete` verifies that ordering.
- Failover model (`.claude/skills/verify-failover/SKILL.md` /
  `FailoverChain.tla`): when an upstream session dies while its route is still
  advertised, the subscriber closes its sources before failing their copies so
  the front classifies the failure as route loss and keeps failover eligible.
  Model source closure before copy failure in `FailoverChain.tla` and keep
  route withdrawal as a separate transition.
- Update the coverage tables and sync commits in both `SKILL.md` files once
  TLC passes.

Tests: run the TLC model checks named in `.claude/skills/verify-resume/SKILL.md`
and `.claude/skills/verify-failover/SKILL.md` (plus `just check`).
