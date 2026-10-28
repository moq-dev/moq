# [M] moq-clock moves into kio::clock

## Goal

moq-clock and moq-clock-macros become `kio::clock` and kio's test attribute
(`#[kio::test]`), behind kio features that mirror moq-clock's today
(`runtime`, `sim`, `manual`, `serde`). kio's private `_sim` feature goes, every
dependent imports `kio::clock`, and both crates are deleted before the cut
publishes them.

## Plan

Decided with the maintainer on 2026-10-25:

- **Why kio.** Waiting and time are the two halves of "easy async": a
  `Deadlines` queue parks kio waiters, and the simulator drives kio tasks.
  They're already coupled through kio's `_sim` feature, which exists only for
  moq-clock's simulator. Zig's `std.Io` carries the clock with its waits for
  the same reason. moq-clock is infrastructure, not a MoQ component, so the
  `moq-` prefix was wrong too.
- **Now.** moq-clock is not on `release`, so the move is free before the cut
  and a break after it.
- **moq-sock stays.** It is synchronous OS socket plumbing tuned for QUIC (a
  quinn-udp fork, connection-id sharding, CPU pinning), not waiting, and would
  pull libc and socket2 into a dependency-light crate that also targets wasm.
- **No generic I/O.** kio gets no socket or file seam: tokio's readiness model
  and io_uring's completion model (the kernel owns the buffer) can't share a
  zero-cost API, and moq's I/O is UDP and TCP under sans-IO cores already split
  per runtime by moq-sock.

Mechanics: keep default features lean, so a kio user that doesn't want a clock
compiles no runtime backend. The 31 dependents, the `clippy.toml` disallowed
methods that name `moq_clock::now()` and friends, and `#[moq_clock::test]`
call sites move over. Move `doc/lib/rs/moq-clock.md` to a kio page (or into
kio's), and fix `doc/lib/rs/index.md`, `doc/setup/repository.md`, and
`doc/setup/upgrade.md`. JS keeps `@moq/clock`: it has no kio, and the
`clock` name still mirrors.

Public API: moq-clock's items move to `kio::clock` unchanged; kio gains
features. Describe the kio migration in the PR's Impact section. Wire: none.

## Related

- [Libraries spawn nothing](/quest/m1/poll-first/no-spawn.md) - removes web-async from the clock's browser backend after this
- [Poll-first async](/quest/m1/poll-first/README.md) - the direction kio carries
