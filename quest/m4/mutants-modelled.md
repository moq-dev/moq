# [L] No mutation of the other modelled code survives its tests

## Goal

The Nightly mutation run that guards the route and resume code (#8712) also
covers the other code a TLA+ model checks: `rs/moq-net/src/reconnect.rs`,
`lite/publisher.rs`, `lite/subscriber.rs`, and `tail.rs`. The tests fail on
every edit to them that changes behaviour.

## Plan

Split from the route and resume run (#8712, taken by the agent under
/quest-afk, for the maintainer's review), which measured what these files
would add. Its quest said they join once its cost was known, and they do not
fit as they are:

- They hold 1681 mutants against the route and resume code's 1300, so the
  rotation (`sh/rs/mutants.sh`) would more than double each night's runner
  time, or need twice the rotation's nights to cover everything once.
- Their survivors from a first full run have to be killed or excluded before
  Nightly can fail on them, as #8712 did for route and resume.
- Their tests are the lite session and reconnect suites, not the route ones,
  so the config needs a second test selection or a second `.cargo/mutants.toml`
  run.

Decide which nights the run gets (more shards, a longer rotation, or a
weekly job) once a first full sweep says how long their tests take per
mutant; then sweep, kill or exclude every survivor, and add them to
`examine_globs`.

Public API: none. Wire: none.

## Related

- [Test coverage audit](/quest/m3/audit-test-coverage.md) - the line-coverage reports this complements
