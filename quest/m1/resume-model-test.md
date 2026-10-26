# [L] Resume checked against a model

## Goal

A randomized test drives resumed fronts through route flips, route failures,
drops, ranges, cache expiry, and reader joins and leaves, and checks every
reader's observed groups, frames, gaps, and end against a simple reference
model of one logical track. It runs in Rust and `@moq/net`, and it passes
before the cut.

## Plan

Decided 2026-10-25 (maintainer): resume must be flawless at the cut. Resume
has had about 18 fixes in its last 31 commits (rs/moq-net/src/model/resume.rs,
2026-10-18 to 10-25), and nothing checks it beyond hand-written route-change
cases, so an oracle comes before any rewrite.

- The model is what one plain cursor on the logical track would see: a
  reader gets every group its range and budget allow, each exactly once, in
  its order, with drops reported per the drop rules. A reported drop is not
  final: a group that arrives later is still served.
- Include a newer publisher epoch winning the path while an older-epoch
  subscription is open: that subscription never sees the new instance's
  groups, and it ends with an error once its own instance has no route.
- Drive it on a mocked clock with seeded randomness, and shrink a failure to
  a minimal sequence that becomes a regression test. Prefer an existing
  property-testing crate.
- Run a fixed seed set in the merge gate and a longer sweep nightly.
- Fix what it finds at the cause. If the failures cluster on the per-reader
  route-copy design rather than single missed checks, bring back the
  [producer-replacement redesign](/quest/m2/resume-redesign.md) to the
  maintainer for m1, with this test as its oracle.

Public API: none. Wire: none.
