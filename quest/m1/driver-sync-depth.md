# [S] Prove the session driver Sync, and moq-tokio's spawn Send, within the default recursion limit

## Goal

`moq_net::Driver: Send + Sync` and moq-tokio's spawned session future prove `Send` at
Rust's default recursion limit, so moq-tokio and any crate that needs `Driver: Sync`, such
as the moq-ffi facade rust2's exporter builds, compile with
`-D recursion_depth_exceeding_limit`. The lint warns today and becomes a hard error in a
later nightly ([rust-lang/rust#159228](https://github.com/rust-lang/rust/issues/159228)).
`just rs send-depth` checks both, so the overflow cannot return. Raising
`recursion_limit` is not a fix.

## Plan

Decided 2026-11-14 (maintainer, in
[kixel-ai/rust2#809](https://github.com/kixel-ai/rust2/pull/809)): ✅ wrap the lite
publisher's `kio::Tasks<Control>` so it proves `Send + Sync` once, as
[#8657](https://github.com/moq-dev/moq/pull/8657) did for the subscriber, and gate moq-tokio
and the facade under the lint (rejected: record the wait only; allow the lint for moq's
crates, which only defers the failure until a nightly makes it a hard error).

Measured 2026-11-18 at `71c6209f` with rust2's `just ffi moq-cpp`, which denies the lint, on
the flake's pinned nightly:

- moq-tokio overflows proving `Send` for the `Instrumented<...>` spawned at
  `rs/moq-tokio/src/client.rs:703`, through `panic::isolate`, `kio::clock::drive`, and
  `moq_net::time::run<moq_net::Driver>`.
- The facade overflows proving `moq_net::Driver: Sync`, through `driver::State`,
  `Protocol`, `lite::session::Driver`, `lite::publisher::Publisher`, and its
  `kio::Tasks<Control>` (`rs/moq-net/src/lite/publisher.rs:264`), into `ControlState` and
  `RequestServe<SubscribeServe>`.

Both chains run through the session driver, so the publisher's proof likely fixes both;
check moq-tokio after it. `sh/rs/send-depth.sh` checks only moq-net's lib and its `send`
test, which proves downstream futures `Send` but never `Driver: Sync`: add that assertion
to the `send` test and check moq-tokio's lib in the same script. The script's nightly is
the flake's pin, so only a pin bump changes the compiler it checks.

Recommended: prove `Send + Sync` at the narrowest type that cuts the chain, with an
`unsafe impl` only where a safe newtype cannot, and a `// SAFETY:` that names what the
wrapped state holds. Measure the gate's added time.

This unblocks rust2's exporter moq pin bump
([kixel-ai/rust2's quest/m1/ffi-refusals/moq-pin.md](https://github.com/kixel-ai/rust2/blob/main/quest/m1/ffi-refusals/moq-pin.md)),
which waits on `just ffi moq-cpp` passing at moq's `main`.

Public API: none. Wire: none.
