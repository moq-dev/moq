# [S] A debug moq-relay starts on Windows

## Goal

A debug (`dev` profile) `moq-relay` starts on Windows, where the main thread
has a 1 MiB stack, instead of dying with `STATUS_STACK_OVERFLOW` before it
logs anything. Embedders that call `Relay::load(..).await` and `.run().await`
from their own Windows main thread are covered too, not only the binary.

## Plan

Reported in [#5563](https://github.com/moq-dev/moq/issues/5563): `cargo run
--bin moq-relay -- prod.toml` in PowerShell or the MinGW console overflows the
main thread, with no output even at `RUST_LOG=trace`. It works under WSL,
which has an 8 MiB main thread.

Likely cause, not yet confirmed: `rs/moq-relay/src/main.rs` is one
`#[tokio::main]` block that inlines `Relay::load`'s future, the `Relay`, and
`Relay::run`'s future into a single unboxed state machine. `block_on` builds
and polls that state machine on the main thread, and debug builds copy it by
value several times. Logging starts inside `Config::load`, which is why
nothing prints. The relay tests already sidestep this with 32 MiB threads
(`rs/moq-relay/tests/goaway_cluster.rs` and others), and
`rs/moq-relay/src/connection.rs` and `rs/moq-tokio/src/client.rs` already box
large futures for the same reason.

Decided 2026-10-15 (maintainer):

- Fix at the source by boxing the large futures inside the library:
  `Relay::load` and `Relay::run` box their large inner futures, following the
  precedent in `connection.rs` and `client.rs`. `main` stays thin. Neither a
  bigger thread in `main` nor a `/STACK` linker flag; both only add headroom
  for the binary.
- Confirm first: measure `size_of_val` of the main future, `Relay::load(..)`
  and `run()` in a debug build, and box what is actually large.
- Regression test: drive the relay's load and run with a minimal config on a
  thread with a 1 MiB stack in a debug build. It must overflow without the fix
  on any OS, so Linux CI catches it too.
- Windows CI adds `-p moq-relay` to `windows-test` (`rs/justfile`), so the
  relay's tests, the regression above included, run on Windows. That needs
  the tests' Unix-only expectations gated first, keeping them on Unix:
  `rs/moq-relay/tests/released_cli.rs` requires the `server-unix-*` flags and
  their env vars, which exist only under `cfg(all(feature = "uds", unix))`
  (`rs/moq-tokio/src/listen.rs`).

No release backport is planned; release builds have smaller futures. If the
fix shows `release` overflows too, ask the maintainer.

Public API: none expected (boxing is internal). Wire: none.

## Closes

- [#5563](https://github.com/moq-dev/moq/issues/5563) - Relay: Stack overflow error launching relay on Windows
