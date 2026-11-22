# [L] Cut Rust just check CPU and mbx cache misses

## Goal

Cut `just check` average CPU cost (~38 CPU-min per run, ~98.5 CPU-h/week on the
build host) and `queue.yml` `mbx` miss/bypass counts (260 misses and 105
bypassed on a full queue run) by at least half without weakening merge-gate
coverage.

## Plan

Host telemetry over 7 days shows `just check` running 156 times for 98.5
CPU-hours (37.9 CPU-min mean, 39.2 CPU-min median, 61.0 CPU-min p90, 709s
average wall), while the two `moq` CI runner containers accumulated ~236
CPU-hours since boot. A full `queue.yml` run logged `mbx[cache]: 135 hits, 260
misses, 48 not looked up, 11 incremental, 105 bypassed`.

- Profile the top `mbx` cache misses and bypasses across `rs`, `rs_cross`,
  `carla_bridge`, and `bench`: inspect `openh264-sys2` compiler flag and
  build-script env sensitivity, `sh/rs/cross-lint.sh`'s `zig-cc.sh` wrapper and
  `nix build .#rust-cross` inputs, `workspace-hack` invalidation, and
  `carla_bridge`'s separate Cargo workspace.
- Audit `sh/rs/select.sh` and `sh/rs/reads.sh` for over-selection so routine
  single-crate edits do not rebuild unrelated dependents or trigger unnecessary
  passes.
- Keep changed crates in `rs_cross` (`aarch64-apple-darwin` and
  `x86_64-pc-windows-msvc`) unless the selector proves their changes cannot
  affect cross targets (so private edits introducing platform-specific APIs are
  still caught), narrow only unchanged dependents without platform `#[cfg(...)]`
  gates or public API changes reaching them, and skip `cargo doc` over unchanged
  dependents when only private code or tests changed.
- Measure weekly `just check` CPU-minutes and `queue.yml` `mbx` hit rate before
  and after.

Decided 2026-11-21 (afk, `/quest-plan`):

- Placement: ✅ `quest/m0/ci/rust-check-cpu.md` (belongs in the active m0 CI
  cost and latency epic; rejected: top-level `m0`, `m3`).
- Approach: ✅ fix `mbx` cache misses and tighten diff selection rather than
  removing cross-platform or rustdoc checks from the gate (rejected: moving
  `rs_cross` to nightly only).
