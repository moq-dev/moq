The `/rs` Cargo workspace. Extends the root `CLAUDE.md`.

# Crates

One crate per component from the root list, named `moq-<component>` (`hang` and `libmoq` are the exceptions). Keep them modular; a crate does one thing. Beyond that list:

- `kio`: "easy async" primitives everything else polls through.
- `moq-native`: configures the QUIC backends (Quinn/Quiche/Noq/Iroh) and the fallback transports for native binaries.
- `moq-cli` builds the `moq` binary and owns the CLI surface for the gateway crates. `moq-token-cli` builds `moq-token`. Binaries never carry a `-cli` suffix.
- `quest` validates the plans under `/quest`.

`moq-net`, `moq-mux`, `moq-relay`, and `moq-ffi` have their own `CLAUDE.md`.

# Producer / Consumer

Most crates use a split-handle pattern: a `Producer` writes, any number of `Consumer`s read, state is shared via `kio`. Handles are refcounted: clones share state, the last drop closes cleanly, `abort(err)` closes with an error. A parked clone that keeps something alive is a bug in the holder.

# Async / poll

Two ways to drive things, both backed by `kio`:
- `poll_*` functions take a `&kio::Waiter` and return `Poll<...>`, drivable from any executor or synchronously. Like `Future`, but wakers are cleaned up on drop.
- `async fn` wrappers, usually `kio::wait(|waiter| self.poll_x(waiter))`.

Prefer poll. New logic is a `poll_*` with an `async` helper, not the other way around. `pin!` then polling inside `kio::wait` is a code smell, as is `tokio::spawn` / `select!` inside a library; use `kio` primitives. Don't color APIs with `Send` bounds that native needs and wasm can't satisfy.

- Use `ready!(...)` instead of `Poll::Pending => return Poll::Pending`.
- `Poll<Result<T, E>>` supports `?`: `ready!(poll())?` or `match poll()?`.
- Prefer `Ok(x?)` over `.map_err(Into::into)`.
- Prefer `kio` over `tokio::sync`. A `watch` or channel carrying a single value is a smell.

# Conventions

- `thiserror` with `#[from]` for libraries; `anyhow` with `.context("...")` for binaries.
- Public error enums are always `#[non_exhaustive]`. Don't re-export a third-party error type unless the crate is a thin wrapper that re-exports the dependency.
- Make terminal operations consume `self` (`fn close(self)`). Rely on `Drop` instead of a `close` the user can forget.
- Take `&[T]` / `&str` for data the callee only reads; return owned values. Take `Vec<T>` only when storing it.
- Typed units only: `std::time::Duration`, `moq_net::Timestamp`, never `f64` seconds or bare `u64` millis. `serde_as` converts at the edge.
- `if let` / `let else` over a `match` whose only job is to bind. Keep `match` when both arms do work.
- Public modules with short names: `broadcast::Consumer`, not `BroadcastConsumer`. Keep `mod encoder` private and re-export flat as `encode::Encoder`.
- Workspace members and shared dependency versions live in the root `Cargo.toml`; crates reference deps via `{ workspace = true }`.
- Binaries: `#[tokio::main]`, install the rustls crypto provider first, then `Config::load()` which sets up tracing. Config conventions live in `moq-relay/CLAUDE.md`.

# Semver

- Don't add `#[non_exhaustive]` by default. It earns its keep on error enums, enums that will gain variants, and `Config`-style structs with `pub` fields plus a `Default`/constructor. Builders with private fields don't need it.
- Append new variants to the end of a public fieldless enum with implicit discriminants; inserting reorders `as` values.
- Use `#[error(transparent)]` + `#[from]` for wrapped foreign errors.
- A deprecated item gets `#[doc(hidden)]` and `#[deprecated(note)]`; a deprecated flag becomes a hidden clap alias. Never advertise the dead name in docs or `--help`.

# Testing

- Tests are inline `#[cfg(test)] mod tests`. Time-dependent async tests call `tokio::time::pause()` first.
- Run tests through `just` (nextest), not `cargo test`: nextest kills a wedged test as TIMEOUT, cargo hangs forever. A test flagged SLOW is a bug to fix, not a threshold to raise.
- `just check` compiles default features only, like CI. `just rs features` (nightly) covers `--all-features` / `--no-default-features`. Keep a feature gate around the dependency, not the logic, so the logic's tests stay in the merge gate.
- Local checks compile only the host platform; `just rs windows` / `macos` must run on that OS and `just rs wasm` covers `moq-wasm`. Say plainly in the PR when platform code is uncompiled.
