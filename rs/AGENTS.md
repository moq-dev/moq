The `/rs` Cargo workspace. Generic Rust rules live in `quest context rust`.

# Crates

- A crate does one thing: component crates are `moq-<component>` (`hang` is the exception); binaries never carry a `-cli` suffix.
- `moq-quic` (our quinn fork) is the only QUIC stack we develop (also powers qmux and WebTransport). Don't add or deepen deps on `web-transport`, `noq`, or `quinn`.
- `moq-cli` builds the `moq` binary and owns the CLI surface for gateway crates.
- `moq-tokio` and `moq-uring` hold runtime plumbing only (dialing, reconnect loops); shared logic belongs in `moq-net`.

# Producer / Consumer & Async (`kio`)

- A `Producer` writes and any number of `Consumer`s read via `kio` (clones share state, last drop closes cleanly, `abort(err)` closes with error). A consumer never keeps its producer or parent alive; a parked clone keeping something alive is a bug.
- Track/group/frame consumers seeing `Error::Dropped` is a bug (find the unfinished producer drop and `abort` with the real cause); lifecycle watchers like `track::Demand` return `Dropped` after `close()` by design.
- New logic is `poll_*(&kio::Waiter) -> Poll<...>` with `#[cfg_attr(feature = "async", kio::wrapper)]` (document what it resolves to, not registration). Builds without `async` compile no coroutines (`just rs coroutines`).
- Fixed-order multi-wait `poll_*` uses `#[kio::machine]` with `wait!`; hand-write when waits sit in loops/branches or yield multiple times. Libraries use `kio`, never `tokio::spawn`, `select!`, `tokio::sync`, or `pin!` inside `kio::wait` (single-value channels are a smell).

# MoQ-Specific Conventions & Testing

- Flushing, cutting, and expiry advance on `max(wall clock, pts)` through a timer (peer timestamps are untrusted and sparse tracks may stall).
- Apply `#[non_exhaustive]` to returned event/item types (including struct variants) as well as extensible enums/configs.
- Renamed flags/env vars stay as hidden args reported via `deprecated()` into `moq_tokio::cli::Deprecated` (failing startup naming the replacement).
- Time-dependent async tests default to `#[kio::test]`. Use `#[tokio::test]` with `tokio::time::pause()` only when needing a real tokio runtime with no socket I/O racing a timer. Real-socket tests call `kio::clock::host_time()` (`just test` sets `KIO_DENY_HOST_TIMERS=1`) and assert lower bounds.
- A SLOW nextest test is a bug to fix, not a threshold to raise. Local checks build host-only; nightly covers `just rs features` and Windows/macOS.
