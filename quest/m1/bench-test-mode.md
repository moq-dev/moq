# [S] moq-net's benches run to completion

## Goal

Every moq-net bench target runs to completion, both in Criterion's test mode
(`cargo test -p moq-net --features fuzz --benches`) and in Nightly's measured
run, so a bench broken by a behaviour change fails where someone sees it.
Done when that command passes on main, Nightly's "Rust bench targets" job gets
past its `criterion net` step, and the PR records where CI catches the next
such break.

## Plan

Planned 2026-11-04 with the maintainer. While iterating
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) (authored tracks
default to zero max age, merged 2026-11-04), its agent ran the Rust benches in
test mode: `group` panicked under the new default and was fixed there, while
`origin`, `session` and `track` in `rs/moq-net/benches/` panicked on main
before the change.

Nightly already runs these targets measured: `sh/gh/nightly.sh`'s `rust-bench`
step runs `criterion net --no-fail-fast -p moq-net --features fuzz --bench '*'`.
Scheduled run
[41656613559](https://github.com/moq-dev/moq/actions/runs/41656613559)
(2026-11-03, before #7729) failed it on four targets:

- `origin` (`origin.rs:574` and `:576`, `winner changed`) in
  `origin/reprice_duplicate/1d_1s_cost2`.
- `session` (`session/mod.rs:371` and `:492`, `Evicted`) in
  `session_delivery_burst/moq-transport-22/...`.
- `memory` (`memory.rs:194`, `Evicted`) in `memory/streams=16/size=1048576`.
- `track` (`track.rs:1718` and `:1730`) in `track_range_widening/1r/64`, fixed
  by [moq-dev/moq#7840](https://github.com/moq-dev/moq/pull/7840) (merged
  2026-11-04).

Start by re-running every moq-net bench target on main, in test mode and in a
short measured run, since #7729's zero max-age default may have broken more.
Fix each at its cause: a bench whose expectation predates a deliberate
behaviour change is updated; a bench exposing a product bug gets the product
fix. `origin` and `session` need `fuzz`, and Cargo skips a bench whose
`required-features` are off without a word, so a test-mode run without it
passes while they're broken.

Where CI catches the next break is the maintainer's call; ask in the PR. A
test-mode run in Nightly adds nothing, since the measured run covers the same
targets. Today the PR gate builds benches with their features but leaves them
unrun (`rs/justfile` above the `test` recipe; `.config/nextest.toml`'s
`default-filter = "not kind(bench)"`). Running them there, which would have
caught these before merge, reverses the
[Nightly epic](/quest/m0/nightly/README.md)'s 2026-10-24 decision that the
gate keeps skipping benches and a green Nightly catches them within a day.
Measure the test-mode run's cost before asking.

Public API: none. Wire: none.

## Related

- [Nightly passes on main](/quest/m0/nightly/README.md) - this quest owns the moq-net bench panics in Nightly's "Rust bench targets" job
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - calibrates the measurements; this quest only keeps the bench executables correct
