# [XS] moq-net builds without async

## Goal

`moq-net` builds, tests, and documents without its `async` feature, and CI
checks that configuration so it stays that way. On main today (2026-11-06),
`cargo check -p moq-net --no-default-features --lib --tests` fails:
`track.rs`'s lib test calls `read_frame` on `group::Consumer`, which only
exists with `async`. The crate's no-async doc links are broken too
(reported by [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)).

## Plan

Decided 2026-11-06 (maintainer, /quest-complete): ✅ m0, fix both and add the
no-async build to CI. Not chosen: m1; fixing it without a CI lane.

Add the configuration to an existing check lane (such as `just check`'s rs
module) rather than a new job, and include the docs build.

Public API: none. Wire: none.
