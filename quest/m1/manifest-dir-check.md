# [XS] Check that Rust code reads CARGO_MANIFEST_DIR at run time

## Goal

`just check` fails on a new `env!("CARGO_MANIFEST_DIR")` in Rust code under
`rs/`, naming the file and line and telling the author to read the variable
at run time with `std::env::var_os`, which cargo and nextest set for tests.
`env!` bakes the checkout's absolute path into the build, so every new
worktree misses mbx's cache for that target. Out of scope: converting more
sites, since `main` has none left to convert.

## Plan

Facts from `main` at 22e3f0ad89:

- [moq-dev/moq#9779](https://github.com/moq-dev/moq/pull/9779) already moved
  every test site under `rs/` (fuzz regressions, interop scripts, the route
  algebra vectors, moq-relay's config tests, and moq-tokio's and moq-uring's
  integration tests) to `std::env::var_os("CARGO_MANIFEST_DIR")`.
  kixel-ai/rust2's build benchmark
  ([bench/build/README.md](https://github.com/kixel-ai/rust2/blob/main/bench/build/README.md),
  [kixel-ai/rust2#1060](https://github.com/kixel-ai/rust2/pull/1060))
  measured the change: a second worktree's test build fell from 685G to 216G
  instructions (-68%).
- What remains: two `compile_fail` doctests in `rs/moq-net/src/util/cell.rs`
  that `include!(concat!(env!("CARGO_MANIFEST_DIR"), ...))`, which need the
  path at compile time, and three interop clients under
  `test/interop/clients/rust/`, which `test/interop/interop.sh` runs as plain
  binaries, where cargo sets nothing at run time.

Decided 2026-11-27 (maintainer, via
[kixel-ai/rust2#1060](https://github.com/kixel-ai/rust2/pull/1060)): ✅ keep
the run-time reads, with a check that catches new `env!("CARGO_MANIFEST_DIR")`
(rejected: no check; drop).

Decided 2026-11-28 (maintainer, /quest-run in kixel-ai/rust2,
[moq-dev/moq#9841](https://github.com/moq-dev/moq/pull/9841)), accepting each
recommendation the headless `/quest-plan` of 2026-11-27 made:

- ✅ The check alone (rejected: converting more sites), since #9779 already
  converted every one the check would flag.
- ✅ A `forbid` line in `sh/text.sh`, which `just check` and the merge queue
  already run (rejected: clippy's `disallowed-macros`, which matches `env!`
  by path and so would ban `env!("CARGO_PKG_VERSION")` too; a custom lint,
  which is far more machinery for one pattern).
- ✅ Scan every `.rs` file under `rs/`, not only test code (rejected: tests
  only). A grep can't tell test code from library code reliably, and library
  code baking in the builder's path is just as wrong. Lines holding
  `include!` stay allowed, which keeps `cell.rs`'s doctests passing.
- ✅ Leave `test/interop/clients/rust/` outside the scan (rejected: having
  `interop.sh` pass the script directory to the clients). Only the interop
  harness builds them, and it would need new plumbing for three small
  examples.
- ✅ m1, since the check is additive and nothing is broken now (rejected:
  m0's CI epic, which is for what is broken or slow today; m4, which would
  leave the gap open while new tests keep landing).

Validation: the check passes on `main` and fails, naming the line, on a
planted `env!("CARGO_MANIFEST_DIR")` in a moq-tokio integration test.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the mbx cache this check keeps warm across worktrees
