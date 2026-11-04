# [S] moq-audio's default-feature test build is clippy-clean and gated

## Goal

`cargo clippy --locked -p moq-audio --tests -- -D warnings` (default
features, test cfg) passes on main, and the PR gate fails a change that
breaks that configuration again. Done when that configuration is clippy-clean
and the gate covers it.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the
[moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652) iterate, which
saw the same finding on its branch and on main.

Verified on `origin/main` at `3c9facb322` (2026-11-03):

```text
$ cargo clippy --locked -p moq-audio --tests -- -D warnings
error: methods `retains` and `fail` are never used
   --> rs/moq-audio/src/encode/producer.rs:517:16
error: could not compile `moq-audio` (lib test) due to 1 previous error
```

Both methods are `#[cfg(any(feature = "capture", test))]`, but their only
callers are in `rs/moq-audio/src/encode/capture.rs`, which compiles only
with `capture`. A test build without `capture` compiles them with no caller.

Why the gate misses it: the land gate lints Rust only through the one
`just rs test` build that `sh/rs/select.sh test` runs, and that build turns on
`moq-audio/capture` and `moq-audio/playback` whenever moq-audio is selected.
So moq-audio's default-feature test build is never compiled in the gate.
[moq-dev/moq#7786](https://github.com/moq-dev/moq/pull/7786), queued on 2026-11-03,
adds `--benches --examples` and each target's `required-features` to the same
build. It keeps the capture picks, so it doesn't catch this either.

- Fix the cfgs: gate `retains` and `fail` on `feature = "capture"` alone, or
  give them a caller outside `capture`. Audit the other
  `cfg(any(feature = "...", test))` items the gate's feature picks hide, in
  moq-audio and moq-video.
- Cover it in the gate without a second full compile, since the gate already
  has only one Rust build (see `rs/justfile`'s `test` recipe comments). One
  way is a lint, for example in `just rs lint`, that refuses
  `cfg(any(feature = "X", test))` on an item whose callers are all behind
  `X`, if that can be checked cheaply. Otherwise, measure the cost of
  linting moq-audio's and moq-video's default-feature `--tests` build when
  they are selected, and bring the numbers to the maintainer if it is not
  small.

Public API: none. Wire: none.

## Related

- [Gate each diff in under five minutes](/quest/m0/ci/five-minutes.md) - the gate's time budget any extra build must fit
