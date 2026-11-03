# [S] The range-widening bench expects a narrowed read to end

## Goal

`rs/moq-net/benches/track.rs`'s `track_range_widening/1r/64` passes on
`main`, under `cargo test -p moq-net --all-targets` and Nightly's `cargo
bench -p moq-net --bench '*'`, with an assertion that matches bounded reads
as [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545) defined them.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [Nightly passes on main](/quest/m0/nightly/README.md)
- m1
- m2

Found by [moq-dev/moq#7536](https://github.com/moq-dev/moq/pull/7536), which
bisected it to #7545 ("end bounded reads",
[64cba13](https://github.com/moq-dev/moq/commit/64cba1337136c67ce77aaf6ae337f355c97a895e)).
After widening, the bench narrows its reader back to the already-delivered
`0..1` and asserts the next `poll_recv_group` is pending
(`bench_range_widening`, around line 1730). Since #7545 the bounded read
ends instead, returning `Ready(Ok(None))`. CI's `cargo test` skips bench
targets, so it went unnoticed.

- Either assert the end, or narrow to a range that isn't finished yet so the
  read stays pending. Pick the one that keeps the bench measuring the widening
  it names, and say why in the PR.
- Confirm with `cargo test -p moq-net --all-targets` and a short `cargo
  bench -p moq-net --bench track -- track_range_widening`.

Public API: none. Wire: none.
