# [S] Release resumes owed groups or proves it doesn't need to

## Goal

A `release` relay's resumed read gets every owed group or a drop for it,
never a hang, if `release` has the hang. Otherwise, this quest records the
evidence that `release` isn't exposed and is deleted.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7708](https://github.com/moq-dev/moq/pull/7708)): milestone m0,
beside the other backports. It started once #7708, the resume fix on main
that this ports, merged.

- Exposure is unproven. `origin/release` (moq-net 0.3.19 at `acf91a00b9`)
  has no `live_floor`, `judge`, or `Misses`. The shared live floor that
  #7708 fixes arrived with
  [moq-dev/moq#4741](https://github.com/moq-dev/moq/pull/4741), which is not
  on `release`. `release` does have `resume.rs`, so
  the symptom may still come through another path.
  `release` also speaks draft 22 (`rs/moq-net/src/ietf/version.rs`), so
  check drafts 14 to 22.
- Reproduce first. Port #7708's regression tests (the cache-loss case and
  the bounded resumes) onto `release`'s older cache model and run them
  there. If none fails, delete this quest and record the run in the PR.
- If one fails, fix it on `release` with the smallest change that matches
  main's contract. Expect a hand-port, not a cherry-pick, since the cache
  models differ. Then cut a patch release under the release line's usual
  rules. Bump the version by hand at the semver level the change needs.
- Keep it separate from [the joined-head FETCH backport](/quest/m0/backport-fill-head-race.md).
  #7708 says the PUBLISH_DONE grace correction tracked there is a separate
  fix.

Public API: none expected. Wire: none.
