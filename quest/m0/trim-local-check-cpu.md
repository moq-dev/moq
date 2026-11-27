# [M] Trim local just check Rust CPU

## Goal

Cut local `just check` Rust CPU by scoping local `cross-lint` to crates with macOS/Windows `#[cfg(...)]` gates, running uncached `cargo doc` in `just ci` / merge queue rather than local `just check`, and replacing compile-time `env!("CARGO_MANIFEST_DIR")` in tests with runtime lookup so `mbx` `share_workspace_root` stops busting across worktrees.

Non-goals: weakening macOS/Windows compile safety or rustdoc warning enforcement before merge to `main`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` after 24h multi-host CPU audit):
- ✅ Scope local `cross-lint` (`just check`) to crates containing macOS/Windows `#[cfg(...)]` gates while keeping full workspace `cross-lint` in `just ci` / the merge queue (rejected: moving `cross-lint` entirely out of local `just check`; keeping full workspace `cross-lint` on every local check).
- ✅ Run `cargo doc` in `just ci` / merge queue only, skipping it in local `just check` (rejected: keeping `cargo doc` in every local `just check`).
- ✅ Replace compile-time `env!("CARGO_MANIFEST_DIR")` in test code with runtime lookup (`std::env::var("CARGO_MANIFEST_DIR")`) where possible so worktree paths do not enter `mbx` action keys.

Baseline (2026-11-26, 2.4h `mbx` session audit on `fedora`):
- `cross-lint` (`aarch64-apple-darwin` + `x86_64-pc-windows-msvc`): 114.2 CPU-min per 2.4h (~19 CPU-h/day on `fedora` alone).
- `cargo doc`: 100% bypassed by `mbx` (494 bypasses in 2.4h).
- `env!("CARGO_MANIFEST_DIR")` appears in ~20 test files across `moq-net`, `moq-mux`, `moq-quic`, `moq-relay`, `moq-rtmp`, `moq-sock`, `moq-tokio`, `moq-uring`, and `qmux`, forcing `mbx` to fall back to absolute workspace paths (~2 CPU-min per build across worktrees).

Approach:
1. In `sh/rs/select.sh` and `sh/dispatch.sh`, distinguish local `just check` from `just ci` (`CI_STRICT=1` or `$action == ci`):
   - In local `just check`, filter `cross` selection to crates whose changed/selected targets actually contain `target_os = "macos"`, `target_os = "windows"`, `windows`, `unix`, or platform-specific deps (or detect via a fast manifest/source scan checked by `sh/rs/select.test.py`), keeping full `cross-lint` in `just ci` and the merge queue.
   - Skip `cargo doc` in local `just check` (`rs/justfile` `lint`), keeping `RUSTDOCFLAGS="-D warnings" cargo doc` in `just ci` and the merge queue.
2. Replace compile-time `env!("CARGO_MANIFEST_DIR")` in Rust tests (`rs/*/src/fuzz.rs`, `rs/moq-net/src/test_interop.rs`, `rs/moq-net/src/route/tests/algebra.rs`, `rs/moq-relay/src/config.rs`, `rs/moq-tokio/tests/integration/*.rs`, `rs/moq-uring/tests/integration/*.rs`) with a shared helper reading `std::env::var("CARGO_MANIFEST_DIR")` at test run time (Cargo and nextest both set it at runtime). Verify `include!`/`concat!` uses that genuinely need compile-time paths stay untouched.
3. Measure before/after local `just check` CPU time and `mbx` cross-worktree hit rate on a 1-line edit to `moq-net`.

## Related

- [Split moq-net unit tests out of rs/moq-net/src](/quest/m1/split-moq-net-tests.md) - reduces `moq-net` compile and downstream rebuild cost
