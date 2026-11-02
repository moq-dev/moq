# [L] Generated async wrappers behind a feature

## Goal

Every `async fn x(..) { kio::wait(|w| self.poll_x(w, ..)).await }` wrapper in
moq's library crates (kio, moq-net, and their poll-based siblings) is generated
from its `poll_*` method, not written by hand, and compiles only with the crate's
`async` cargo feature. With the feature off, the crate compiles no coroutines
(`async fn` bodies). moq's binaries, tests, tokio glue, and the bindings enable it,
and the bindings and JS keep their APIs.

## Plan

Decided with the maintainer on 2026-10-25: poll is canonical, async is the
ergonomic layer, and the gating proceeds regardless of the build measurement.

Decided with the maintainer on 2026-11-01, from the
[generator spike](https://github.com/moq-dev/moq/blob/91b1ec62e1e35295ef74d0f7cee1afc49739bf00/bench/async-wrappers/RESULTS.md):
the generator is a method attribute in kio-macros, `#[kio::wrapper]`. The poll
stays ordinary Rust, the wrapper's name, signature, and docs are derived from it,
and a standalone kio build with `async` pays the measured proc-macro cost (about
3.5 seconds clean).

Done in [moq-dev/moq#7506](https://github.com/moq-dev/moq/pull/7506):
`#[kio::wrapper]` with its contract in kio-macros' behavior and UI tests; kio's
default-off `async` feature, under which kio compiles no coroutines; moq-net's
`async` enabling `kio/async`; moq-net's four `broadcast` wrappers generated; and
`just rs coroutines`, in the nightly feature matrix, which scans the MIR of kio's
and moq-net's poll-only builds. Callers gate the attribute with
`#[cfg_attr(feature = "async", kio::wrapper)]`, so a poll-only build skips the
proc macro, and the scan catches a wrapper left ungated.

Checkpoint in [moq-dev/moq#7607](https://github.com/moq-dev/moq/pull/7607):
moq-json's six wrappers are generated behind its default-off `async` feature;
its consumers in moq-mux, moq-stats, moq-cli, moq-boy, moq-ffi, moq-c, and
moq-net-tests enable it. The migration guide and coroutine matrix cover it.
The coroutine scan still needs validation: the local mbx Cargo launcher fails
with `Argument list too long (os error 7)` before rustc.

Decided with the foreground on 2026-11-01: sequence the overlapping origin
driver conversion after the instance API in
[moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489) and the related serve
API changes land. Do not integrate their unmerged heads. They change the
`serve_front` state, source selection, and request/follow signatures that this
quest must convert. Independent wrappers and feature propagation can proceed.

What remains:

- **moq-net's remaining wrappers:** about 47 hand-written
  `kio::wait(|waiter| self.poll_x(waiter, ..)).await` wrappers become the attribute
  on their polls, each poll's docs rewritten to read for both forms. A few stay
  hand-written: those that take a closure by value and poll it by `&mut`, and the
  `#[cfg(test)]` helpers the poll-only tests use.
- **moq-net's poll-only coroutines:** `auth::until` (behind `Grant::guard`), and
  `model::origin`'s `run_front` and `serve_front`, still compile without `async`.
  Gate `until` with its async callers, or make it a poll, and turn the front
  driver into a hand-written poll machine (its body is one large `kio::wait`).
  Then drop their `--allow`s from `just rs coroutines`.
- **Logic in `async fn` bodies:** an `async fn` that does more than call one
  `poll_*` becomes a `poll_*` first, or moves out of the library to the edges. List
  each one in the PR. moq-net's straight-line ones became `#[kio::machine]`s in
  [moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284), except those
  that await a looped body with no `poll_*` yet, such as `Server::accept`; the
  bodies with a loop or a branch around a wait (about 59 in moq-net) are
  converted by hand, since the macro stays straight-line only (decided
  2026-10-28).
- **Sibling libraries:** the poll-based libraries with hand-written wrappers
  (moq-mux, moq-hls, moq-auth, moq-stats, moq-room, moq-sign, moq-e2ee,
  qmux, and others) gain an `async` feature, generate their wrappers, and join
  `just rs coroutines`.
- **Default off:** moq-net's `async` feature is still on by default. Turn it off,
  and enable it at the edges: binaries, tests, tokio glue, and the bindings.
- **Tests:** `#[kio::test]` and `#[tokio::test]` bodies stay async, so test
  targets enable the feature.
- **Cargo:** feature unification means one dependent enabling `async` enables it
  for the whole build. Make sure library-only builds, such as `cargo check -p
  moq-net` and the rust2 bench's library-only build, leave it off.

Public API: every library crate gains an `async` feature, and its async wrappers
move behind it. Add an Impact migration note for the published crates whenever
a consumer must enable `async`, including with `default-features = false`, as
`rs/kio/MIGRATE.md` does for kio.
