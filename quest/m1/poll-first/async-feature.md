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

- **Spike the generator first** on kio and one moq-net module, then pick one:
  - a `macro_rules!` over a list of signatures (no proc-macro cost);
  - a proc macro attribute on the `poll_*` method or its impl block;
  - a rust2-driver lowering.
  Compare compile time (clean and edit, on moq-net), error spans, and docs output.
  A wrapper's docs come from its `poll_*` method.
- **Logic in `async fn` bodies:** an `async fn` that does more than call one
  `poll_*` becomes a `poll_*` first, or moves out of the library to the edges. List
  each one in the PR. moq-net's straight-line ones became `#[kio::machine]`s in
  [moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284), except those
  that await a looped body with no `poll_*` yet, such as `Server::accept`; the
  bodies with a loop or a branch around a wait (about 59 in moq-net) are
  converted by hand, since the macro stays straight-line only (decided
  2026-10-28).
- **Tests:** `#[kio::test]` and `#[tokio::test]` bodies stay async, so test
  targets enable the feature.
- **Cargo:** feature unification means one dependent enabling `async` enables it
  for the whole build. Make sure library-only builds, such as `cargo check -p
  moq-net` and the rust2 bench's library-only build, leave it off.
- **Docs:** update `rs/AGENTS.md`'s rule ("New logic is a `poll_*` with an `async`
  helper") to say the helper is generated and feature-gated.

Public API: every library crate gains an `async` feature, and its async wrappers
move behind it. Add an Impact migration note for the published crates whenever
a consumer must enable `async`, including with `default-features = false`.
