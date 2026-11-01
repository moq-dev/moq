The `/rs` Cargo workspace. Extends the root `AGENTS.md`.

# Crates

Component crates are named `moq-<component>` (`hang` is the exception); infrastructure like `kio` is not. Keep them modular; a crate does one thing. The ones whose role isn't obvious from the name:

- `kio`: "easy async" primitives everything else polls through.
- `moq-tokio`: configures the QUIC backends (`moq-quic`, our quinn fork, and Iroh) and the fallback transports for native binaries, on tokio. `moq-sock` holds the socket plumbing it shares with `moq-uring`, the experimental thread-per-core io\_uring runtime.
- `moq-quic` is the only QUIC stack we develop; it also powers qmux and WebTransport. Don't add or deepen dependencies on web-transport, noq, or quinn.
- `moq-cli` builds the `moq` binary and owns the CLI surface for the gateway crates. Binaries never carry a `-cli` suffix.

`moq-net`, `moq-mux`, `moq-relay` (including its layered config), and `moq-ffi` have their own `AGENTS.md`.

# Producer / Consumer

Most crates use a split-handle pattern: a `Producer` writes, any number of `Consumer`s read, state is shared via `kio`. Handles are refcounted: clones share state, the last drop closes cleanly, `abort(err)` closes with an error. A parked clone that keeps something alive is a bug in the holder, and a consumer never keeps its producer or parent alive. A track, group, or frame consumer seeing `Error::Dropped` is a bug too: its producer went away unfinished without calling `abort` with a specific error, so find that path and abort with the real cause. Lifecycle watchers like `track::Demand` return `Dropped` after `close()` by design.

# Async / poll

Two ways to drive things, both backed by `kio`:

- `poll_*` functions take a `&kio::Waiter` and return `Poll<...>`, drivable from any executor or synchronously. Like `Future`, but wakers are cleaned up on drop.
- `async fn` wrappers, usually `kio::wait(|waiter| self.poll_x(waiter))`.

Prefer poll. New logic is a `poll_*` with an `async` helper, not the other way around. `pin!` then polling inside `kio::wait` is a code smell, as is `tokio::spawn` / `select!` inside a library; use `kio` primitives. Don't color APIs with `Send` bounds that native needs and wasm can't satisfy.

- A `poll_*` that waits on several things in a fixed order is a `#[kio::machine]` (kio's `macros` feature): straight-line code with a `wait!` at each wait, from which it writes the state enum and its transitions. Hand-write the machine when a wait sits in a loop or branch, or when it yields more than once (a stream's `poll_next`). A conditional wait is one whose poll is ready at once.
- Use `ready!(...)` instead of `Poll::Pending => return Poll::Pending`.
- `Poll<Result<T, E>>` supports `?`: `ready!(poll())?` or `match poll()?`.
- Prefer `Ok(x?)` over `.map_err(Into::into)`.
- Prefer `kio` over `tokio::sync`. A `watch` or channel carrying a single value is a smell.

# Conventions

- `thiserror` with `#[from]` for libraries; `anyhow` with `.context("...")` for binaries.
- Don't re-export a third-party error type unless the crate is a thin wrapper that re-exports the dependency.
- Make terminal operations consume `self` (`fn close(self)`). Rely on `Drop` instead of a `close` the user can forget.
- Take `&[T]` / `&str` for data the callee only reads; return owned values. Take `Vec<T>` only when storing it.
- Typed units only: `std::time::Duration`, `moq_net::Timestamp`, never `f64` seconds or bare `u64` millis. `serde_as` converts at the edge.
- Flushing, cutting, and expiry advance on max(wall clock, pts) through a timer: peer timestamps are untrusted, and a sparse track may never send another frame.
- `if let` / `let else` over a `match` whose only job is to bind. Keep `match` when both arms do work.
- Derive `Serialize`/`Deserialize`; a hand-written impl is a second copy of the wire shape. Reach for `serde_with` for what the derive can't express.
- Public modules with short names: `broadcast::Consumer`, not `BroadcastConsumer`. Keep `mod encoder` private and re-export flat as `encode::Encoder`.
- Workspace members and shared dependency versions live in the root `Cargo.toml`; crates reference deps via `{ workspace = true }`, except internal dev-dependencies, which are path-only so releases publish.
- Use newtypes and enums instead of untyped strings.

# Semver

- Don't add `#[non_exhaustive]` by default. It earns its keep on error enums, enums that will gain variants, and `Config`-style structs with `pub` fields plus a `Default`/constructor. Builders with private fields don't need it.
- Append new variants to the end of a public fieldless enum with implicit discriminants; inserting reorders `as` values.
- A renamed flag or env var stays as a hidden arg with its original `env`, reported by the section's `deprecated()` into `moq_tokio::cli::Deprecated`; callers must reject startup when the collection is non-empty and name the replacement rather than honor the old spelling. Never advertise the dead name in docs or `--help`.

# Features

- Defaults skip anything expensive to build or needing a system library. Prefer fewer features: each one trims a build but fragments the code and its test matrix, so add one only for a vendor integration or a system dependency.
- A feature lives in the crate that owns the integration (`s3` in moq-archive). Dependents forward it under the same name rather than enabling their dependencies' features directly.

# Testing

- Tests are inline `#[cfg(test)] mod tests`, except full Client/Server session tests over a crate's shared mock harness, which go in its `tests/integration/` module. Time-dependent async tests call `tokio::time::pause()` first (moq-net uses `#[moq_clock::test]` instead), unless they cross real networking that can't be mocked (sockets, smoke tests); those run on the wall clock and assert lower bounds.
- Run tests through `just` (nextest), not `cargo test`: nextest kills a wedged test as TIMEOUT, cargo hangs forever. A test flagged SLOW is a bug to fix, not a threshold to raise.
- `just check` and CI build and test the selected crates in one build at their default features, plus the off-by-default device features and, on Linux, the relay's `io-uring` feature. `just rs features` (nightly and release PRs) covers the feature matrix. Keep a feature gate around the dependency, not the logic, so the logic's tests stay in the merge gate.
- Local checks compile only the host platform; Nightly and release PRs run `just rs windows` / `macos` on those hosts and build `moq-wasm` for wasm32. Run `just rs wasm` after touching code it compiles.
