`kio` (with `kio-macros` in `macros/`) is moq's poll-native async shared state, deterministic clock and simulator, and state-machine macros. It is published on its own and used outside moq.

# Required

- Every wait is a `poll_*` method first. The `async` feature adds `async fn` wrappers (via `#[kio::wrapper]`); the default build compiles no coroutines and no host timer runtime (`just rs coroutines`).
- Keep `kio` free of MoQ assumptions: no MoQ names, epochs, or protocol rules. Those live in `moq-net`.
- Every `unsafe` block states its safety invariant inline and has tests exercising the ordering or lifetime rule it relies on.
- Unit tests use `kio::clock::sim` or `kio::clock::Manual` instead of sleeping on the host clock, except explicit host-clock tests gated for Miri.
- A breaking change gets an entry in `MIGRATE.md` (or `macros/MIGRATE.md` for `kio-macros`).

# Verified code

`src/log.rs` carries a Verus proof of its append and read paths, in Verus's attribute syntax so the file stays plain Rust. Only Verus sets the `verus_keep_ghost` cfg that turns the proof on, and only the `verify/` package, which builds kio's sources for `just rs verify`, depends on `vstd`. Plain builds and downstream crates compile no Verus crate, and editing the log needs no Verus. Keep `verify/Cargo.toml`'s dependencies in step with kio's non-optional ones. Nightly proves it; to run it locally, `nix develop .#verus --command just rs verify` (Linux only).

- Contracts sit in `#[cfg_attr(verus_keep_ghost, verus_spec(...))]`; proof steps in `proof!`, ghost `let`s in `proof_decl!`, and a call's ghost arguments in `proof_with!` just before it, so inside the `unsafe` block of a call to an `unsafe fn` (inside `open_atomic_invariant!`, a `verus_spec(with ...)` attribute on the statement instead). Ghost types, spec functions, and lemmas live in `verus!` blocks under `#[cfg(verus_keep_ghost)]`, and ghost fields under `#[cfg(verus_keep_ghost_body)]`. Plain builds expand all of these to nothing (`src/log/erased.rs`).
- Every atomic the proof tracks goes through `src/log/atomic.rs` and is accessed inside `open_atomic_invariant!`, one atomic operation per block.
- A change that publishes `filled` before writing its slot, passes an order weaker than `Release` to a tracked store, reads a slot past `filled`, or skips a ghost update fails `verify`, not the merge gate. Verus does not check the bodies of the `external_body` atomic wrappers in `src/log/atomic.rs`, so weakening an ordering inside one passes `verify`. The proof is safety-only: storing `end` before `filled`, which makes a reader report a committed entry `Evicted`, still passes. Fix the code or extend the proof; never add an `assume`, and mark any new trusted (`external_body`) function `TRUSTED` with why.
