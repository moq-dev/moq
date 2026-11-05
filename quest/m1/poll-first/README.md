# [XL] Poll-first async

## Goal

moq's library crates write asynchronous logic as kio `poll_*` functions and
compile no coroutines by default. Their `async fn` wrappers are generated and
behind an `async` cargo feature that binaries, tests, and tokio glue enable. A
design report says how far to take it: whether a kio proc macro should write
`poll_*` bodies from straight-line code, and whether moq-net's protocol core
should become sans-IO. Everything stays interoperable with the Rust ecosystem at
the edges, through std's `Waker` and `Context`.

## Plan

Decided with the maintainer on 2026-10-25, from a survey of async Rust's problems
and their alternatives:

- **Problems:**
  - function coloring;
  - `Pin` and `Unpin`;
  - cancel safety and no async drop;
  - `Send` inference and the `Send` bound problem;
  - `'static` spawn;
  - runtime coupling;
  - future size (a 32 MiB test stack in moq-relay);
  - debugging;
  - compile cost: rmeta encodes every `async fn` body's coroutine MIR, so any
    body edit changes the crate's metadata and rebuilds its dependents.
- **Upstream won't fix them soon:**
  - return type notation stalled in Dec 2025;
  - pin ergonomics was discontinued in Aug 2026;
  - async drop is nightly-only, and the 2026 roadmap keeps destructors sync;
  - scoped spawn waits on guaranteed destructors (2026-27).
- **kio already avoids:** `Pin` in user code, hidden cancellation points (progress
  lives in named fields), leaked wakers, runtime coupling in the library, and
  per-future allocation in `Tasks`.
- **What kio keeps:** coloring (`poll_x` plus `async fn x`), `'static` spawn,
  and hand-written state machines.
- **Rejected:** stackful coroutines (green threads). They break wasm, rust2's MIR
  TypeScript translation, and thread-local soundness.
- **Zig's model** (I/O passed as an explicit `std.Io`, like an allocator) removes
  coloring only because its evented backend parks whole stacks. kio's `&Waiter`
  parameter is the stackless half of that idea. Sans-IO is the other: the protocol
  never does I/O and each runtime drives it.
- **Sans-IO compared** in [moq-dev/moq#7058](https://github.com/moq-dev/moq/pull/7058),
  on the lite subscriber's upstream SUBSCRIBE (an unmerged prototype, since deleted).
  moq-net already takes I/O through its transport traits and runs on any executor,
  so sans-IO only buys a core without waiters. That core translated through rust2-ts
  with 4 refusals, but the serve loop driving it still had 919, so translating a
  session still waits on the model. Rules became plain `#[test]`s, while
  interleaving bugs stay in driver tests. Compile cost and the benches were flat.
  The cost is an effect enum per model touch (+206 lines on this path).
  **Decided (maintainer, 2026-10-28):** moq-net's protocol core stays kio poll and
  does not move toward sans-IO. Pull pure wire rules into plain functions where
  that turns scripted async tests into plain ones, as ordinary refactors.
- **Gate the wrappers regardless** of the build measurement, because poll-first is
  the style moq wants. moq-dev/rust2's
  [semantic API hash quest](https://github.com/moq-dev/rust2/blob/main/quest/m3/api-cutoff.md)
  replays moq-net's history with and without the wrappers, to report the build
  effect.
- **Adopt `#[kio::machine]` for straight-line code only**, decided with the
  maintainer on 2026-10-28 from the spike in
  [moq-dev/moq#7057](https://github.com/moq-dev/moq/pull/7057). It lands with its
  first users; loop support waits until conversions show it's needed. The spike
  converted TrackInfoFetch (108 to 78 lines, no `mem::replace` transitions). Its
  type-check and borrow-check cost matches hand-written code; it adds about 1.5 ms
  of expansion per machine plus a one-time 1.3 s build of `kio-macros`, below what
  async costs. The cost is a dialect: stored locals need type annotations and are
  `&mut` inside a wait's poll, and a `!Send` error names the type and the wait but
  not the local: the generated state keeps Rust's automatic `Send`, with no
  `unsafe impl` (decided 2026-10-28). Its users are moq-net's 23 straight-line multi-await `async fn` bodies and 12 straight-line
  hand-written machines; 59 bodies await in loops or selects.

Done in [moq-dev/moq#7506](https://github.com/moq-dev/moq/pull/7506) and
[moq-dev/moq#7607](https://github.com/moq-dev/moq/pull/7607): `#[kio::wrapper]`
generates the async wrapper from its poll, and kio, moq-net, moq-json, hang,
moq-mux, moq-hls, moq-room, moq-stats, moq-crypto, and moq-auth each keep theirs
behind a default-off `async` feature that binaries, tests, tokio glue, and the
bindings enable. `just rs coroutines` proves each poll-only build compiles no
coroutines. Runtime glue (moq-tokio, moq-uring) and bindings generate theirs
unconditionally.

Decided 2026-11-05 (maintainer, /quest-unblock): convert every remaining
multi-await `async fn` body in
[moq-dev/moq#7607](https://github.com/moq-dev/moq/pull/7607) before it lands,
rather than landing it as is and splitting them into child quests. That PR does:
moq-net's sessions (the IETF publisher, subscriber, session driver, and auth,
`Client::connect`, the server handshake), its model readers, the siblings' gated
constructors, and moq-hls's export drivers are poll machines. Outside the HTTP
edges (moq-auth's client, server, and mesh; moq-hls's import and HTTP server), no
library `async fn` awaits more than once.

What remains:

- **moq-hls's import driver** (`import.rs`) still awaits reqwest requests,
  `tokio::fs`, and sleeps between HTTP retries, and its axum routes are async
  handlers. Whether the import driver stays async as HTTP edge glue or becomes a
  poll machine over boxed reqwest futures is open in
  [moq-dev/moq#7607](https://github.com/moq-dev/moq/pull/7607).
- **Wrappers the generator cannot take** stay hand-written: a name that differs
  from its poll (moq-tokio `state_changed`, `presence_changed`; moq-uring `read`,
  `write`), a by-value receiver, a free fn, or a private poll (moq-net
  `Server::close`, `Session::{close, closed, peer_implementation}`, the auth
  handles' `closed`).
