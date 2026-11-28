# [M] Model source closure before copy failure in FailoverChain.tla

## Goal

`FailoverChain.tla` models a dead upstream session's source closure before its
copies fail, as a transition separate from route withdrawal, and its configs
still finish in a few minutes. Then advance the `verify-failover` sync marker
and Coverage rows past the commits it audits.

## Plan

Found 2026-11-27 by replaying `gpt-6.2-sol` (`medium`) on
[moq-dev/moq#9627](https://github.com/moq-dev/moq/pull/9627); split from
`quest/m1/verify-resume-failover-sync.md` on 2026-11-28 (maintainer decision on
[moq-dev/moq#9836](https://github.com/moq-dev/moq/pull/9836)).

- Caused by: [moq-dev/moq#9627](https://github.com/moq-dev/moq/pull/9627)
  (list under `## Caused by` in the fixing PR).
- Code: `SubscriberDriver::abort` closes each source before
  `SubscriptionCleanup::abort` fails its copies, so the front reads each failure as
  its route's loss. A failure taken from a still-open source is the track's verdict
  (`Ended::Aborted` in `front.rs`).
- A first attempt is commit `d973721d96` on the branch of #9836
  (`git show d973721d96 -- rs/moq-net/verify/failover`): `alive[r]` runs
  `up` -> `closed` -> `shown` -> `down` through `Die` (sources close, the relay's
  own copies vanish), `Fail` (downstream copies fail), and `Withdraw`; a `Verdict(u)`
  step takes a failed serving copy of a still-open source as the front's end; and
  `FailsOnlyUnroutable` (`front[u].end = "err"` implies no route of `u` still
  shows) joins every config. The mutant that fails copies before closing sources
  violates `FailsOnlyUnroutable` in 76 states, so the rule has teeth.
- The problem: the split adds interleavings (a `Switch` between closure and
  failure, with the old copy live but starved). `FailoverChain.cfg` passed 20M
  distinct states after 25 minutes without finishing (7.5M before), and `Deaths.cfg`
  passed 24M; `Leave.cfg` and `Mesh.cfg` were not run.
- Design note: fold `Fail` into the step that observes it. The front's reaction
  to the closure and the failure of the copies it carried are one observation
  in `front.rs`, so keep `Die` as the closure (`alive` -> `closed`) and fail a
  copy only inside the steps that read it (`Recover`, `Abort`, `FrontFail`,
  `Verdict`, `Switch`), deriving "failed" from `alive[e[2]] # "up"`. That drops the
  extra `Fail` transition and its interleavings. Measure each config's states
  against the old counts (7.5M, 8.0M, 11.6M) before settling.
- Update the Coverage rows and sync commit in `.claude/skills/verify-failover/SKILL.md`
  once TLC passes, auditing `git log 7cab21e65b.. -- <the skill's files>`
  (#9748, a short close on a copy whose route failed, looks in scope).

Tests: the four configs in `.claude/skills/verify-failover/SKILL.md` (Mesh as its
simulation) plus the mutant above, and `just check`.
