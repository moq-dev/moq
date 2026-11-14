# [L] No mutation of the route and resume code survives its tests

## Goal

cargo-mutants changes `rs/moq-net/src/route/` and
`rs/moq-net/src/model/resume.rs` one small edit at a time, and the tests fail
on every edit that changes behaviour. Nightly runs it and fails on any
survivor, so logic that lands without a test that would notice it breaking is
caught the next morning.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net). Milestone m4. Coverage says which lines ran; this says whether a
bug on those lines would fail anything, for the two modules behind most
production issues.

- ✅ Nightly fails on any survivor (rejected: a report the test coverage
  audit reads, which nothing forces anyone to open; loom faded the same way).
  This PR first kills or excludes every survivor of the first run.
- A survivor is killed with a test, or excluded by a pattern in
  `.cargo/mutants.toml` with a comment saying why the edit cannot change
  behaviour. Prefer the config file over `#[mutants::skip]`, which needs a
  dependency. An edit that makes the tests hang counts as caught.
- Run only the tests that exercise the two modules, with the sweeps on fixed
  seeds: a mutant caught by one random seed and missed by the next would make
  the job flaky.
- Shard it across a Nightly matrix (`--shard k/n`) and report the mutant
  count, survivors found, and wall time in the PR. If the full run does not
  fit Nightly, say what does and rotate the rest.
- The other modelled code (`reconnect.rs`, `lite/publisher.rs`,
  `lite/subscriber.rs`, `tail.rs`) joins once this run's cost is known; say
  in the PR whether it fits.

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): ✅ m1
(rejected: staying in m4). These two modules are behind most production issues.

Public API: none. Wire: none.

## Related

- [Test coverage audit](/quest/m3/audit-test-coverage.md) - the line-coverage reports this complements
- [Resume as producer replacement](/quest/m4/resume-redesign.md) - rewrites `resume.rs`; whichever lands second carries the other's survivors
