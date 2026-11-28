# [M] Clean up pre-0.22 public API leaks in moq-relay, moq-stats, and moq-quic

## Goal

Before `0.22.0` freezes public Rust APIs: `moq_relay::cluster::Shared` and `impl Deref<Target = Shared> for Cluster` are private/removed, `moq_stats::Producer::new` and `aggregate::Consumer::new` return `Result` on zero durations instead of panicking, and `moq_quic::Connection::poll_quiesce` and `abandon_quiesce` are hidden or feature-gated so their planned removal in `m5` is not a breaking `moq-quic` change.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit):
- ✅ Clean up all three pre-0.22 public API shapes (`moq-relay`, `moq-stats`, `moq-quic`), and confirm [moq-dev/moq#9793](https://github.com/moq-dev/moq/pull/9793)'s `broadcast.meta()` / `Meta.*` API shape stays as-is for `0.22.0` (rejected: keeping `moq-stats` panicking; leaving all shapes as-is).

Caused by [moq-dev/moq#9832](https://github.com/moq-dev/moq/pull/9832) (AFK), [moq-dev/moq#9835](https://github.com/moq-dev/moq/pull/9835) (AFK), and [moq-dev/moq#9518](https://github.com/moq-dev/moq/pull/9518).

1. **`moq-relay` (`#9832`):**
   - In `rs/moq-relay/src/cluster.rs:1531-1566` (`rs/moq-relay/src/lib.rs:18`), make `struct Shared` private, delete `impl Deref<Target = Shared> for Cluster`, and access fields via `self.shared.<field>` inside `cluster.rs`.
2. **`moq-stats` (`#9835`):**
   - In `rs/moq-stats/src/produce.rs:188-189` and `rs/moq-stats/src/aggregate.rs:133-134`, return `Result<Self, Error>` (rejecting zero `interval` or `grace` cleanly like `moq-quic`, `moq-tokio`, and `qmux`) instead of panicking in `new()`. Update callers (`rs/moq-relay`, tests) and record the migration note in `## Impact`.
3. **`moq-quic` (`#9518`):**
   - In `rs/moq-quic/src/connection/mod.rs:538,571`, mark `Connection::poll_quiesce` and `Connection::abandon_quiesce` `#[doc(hidden)]` (or gate behind an internal workspace feature) so [chromium-close-quiesce](/quest/m5/chromium-close-quiesce.md) can delete this temporary Chrome workaround without a semver break on `moq-quic`.
   - Note: [moq-dev/moq#9586](https://github.com/moq-dev/moq/pull/9586) already owns `#9518`'s unaccepted bi-stream FIN fix and WebTransport-only code-0 `peerEnded` fix (and its Follow-ups track the late local open race). Do not duplicate `#9586` here.

Tests: `cargo nextest` across `moq-relay`, `moq-stats`, `moq-quic`, `moq-tokio`, and `moq-uring`, plus `just check` (including rustdoc).

Public API: breaking pre-cut cleanup in `moq-relay`, `moq-stats`, and `moq-quic` (PR title uses `!` and `## Impact` carries the migration notes). Wire: none.

## Related

- [Remove WebTransport close quiesce once Chromium fixes 589817008](/quest/m5/chromium-close-quiesce.md) - deletes the hidden `poll_quiesce` workaround later
- [moq-stats stops stalling on a failed node subscribe](/quest/m1/stats-aggregate-fixes.md) - fixes the remaining `Waiting` stall in `moq-stats`
