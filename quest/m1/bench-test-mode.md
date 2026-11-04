# [S] moq-net's benches run to completion in test mode

## Goal

Every moq-net bench target runs to completion in Criterion's test mode
(`cargo test --benches`, or `cargo bench -- --test`), and CI runs them that way
often enough that a bench broken by a behaviour change fails before Nightly's
measurements do. Done when the `origin`, `session` and `track` benches pass in
test mode on main and the PR names the CI job that keeps them passing.

## Plan

Planned 2026-11-04 with the maintainer. While iterating
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) (authored tracks
default to zero max age), its agent ran the Rust benches in test mode: `group`
panicked under the new default and was fixed there, while `origin`, `session`
and `track` in `rs/moq-net/benches/` panicked the same way on main before the
change.

- [moq-dev/moq#7840](https://github.com/moq-dev/moq/pull/7840), queued
  2026-11-04, fixes one `track` panic (`track_range_widening/1r/64` expected a
  bounded read to stay pending after its groups were delivered). Start by
  re-running the three on main after it lands and fix what's left, each at its
  cause: a bench whose expectation predates a deliberate behaviour change is
  updated; a bench exposing a product bug gets the product fix.
- Today the gate only builds bench targets (`rs/justfile`: nextest's default
  filter leaves them unrun), and Nightly's "Rust benchmark tests" job runs
  nextest's ignored benchmark tests, not the Criterion targets. Add a test-mode
  run where it costs least, likely Nightly beside the benchmark tests, and say
  in the PR why it's there and not the gate.
- Related owners: [bench CI](/quest/m1/bench-ci.md) calibrates measurements;
  this quest only keeps the bench executables correct.

Public API: none. Wire: none.
