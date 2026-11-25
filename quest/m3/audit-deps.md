# [M] Audit dependencies

## Goal

Run the `deps` audit: `quest audit deps` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-23 at `890ea4e531` (before the 0.22.0 cut). Both `cargo deny check` and `sh/js/audit.sh` (9 Bun projects) reported 0 vulnerabilities; quinn-rs/quinn had 0 new GHSA advisories since 2026-09-30.

### Check first

- Research each candidate's maintenance, performance, and API fit before a swap. Manual bumps respect Dependabot's 7-day cooldown; security fixes bypass it.
- What Dependabot misses: majors and 0.x minors (`cargo upgrade --dry-run --incompatible allow --pinned allow --ignore-rust-version`, `bun outdated -r`, and standalone Bun projects), `0.0.x` ranges and `npm:` aliases, and lockfiles missing from `.github/dependabot.yml` (`sh/go/modzip/go.sum`, `demo/carla/bridge/Cargo.lock`, `bench/fanout/session/Cargo.lock`; `dart/moq` and `nix/uniffi-dart-Cargo.lock` are uncovered on purpose).
- Held-back / open majors from the 2026-11-23 baseline:
  - Rust: `web-transport-proto` `0.6.4 -> 0.7.0`, `str0m` `0.25 -> 0.26.0`, `jsonwebtoken` `11 -> 12.0.0`, `mdns-sd` `0.21 -> 0.22.0`, `nix` `0.31 -> 0.32.0`, `prometheus-client` `0.25 -> 0.26.0`, `notify` `8 -> 9.0.0`, `tower-http` `0.6 -> 0.7.1`, `winnow` `0.7 -> 1.0.5`, `syn` `2 -> 3.0.10` (held until `thiserror`/`tracing` move), `ashpd` `0.13 -> 0.14.3` (held for Ubuntu 24.04 `xdg-dbus-proxy`).
  - JS: `solid-js` `1.9.19 -> 2.0.3`, `fallow` `3.44.0 -> 3.47.0`, `@types/web` `0.0.358 -> 0.0.367` (check if `WebTransportConnectionStats` landed so `js/net/src/connection/stats.ts` can drop its local type).
- The QUIC fork has no advisory feed: list `quinn-proto` and `quinn-udp` advisories on RustSec and GHSA, port any fix, and note it in `rs/moq-quic/README.md` (decided 2026-10-28). Only a [vendor sync](/quest/m3/vendor-sync/quinn.md) advances the recorded upstream commit.
- Waiting on upstream: [askama](/quest/m5/askama-maintained.md); root Bun overrides (`chokidar@3`, `glob`, `minimatch`, `postcss-selector-parser@6`, `undici@7`, `vite@5`); `deny.toml` unmaintained ignores (`paste` via `tikv-jemalloc-ctl`, `proc-macro-error2` via `core-models`, `ttf-parser` via `winit`, `basic-toml` via `askama`, `zvariant*` via `ashpd`, `chrono`/`humantime` via `object_store` and `qlog`).
- Most duplicate versions in `Cargo.lock` come from upstream ecosystem transitions; check the consumer before calling one removable. Fix a transitive Bun advisory with a targeted `bun update <pkg> --lockfile-only --ignore-scripts`, not a regenerated lockfile.
- Kept on purpose: `tracing-android`, `humantime-serde`, `hex`, `send_wrapper`, `byteorder`, `assert_matches`, `cpu-time`, `stats_alloc`, `unsafe-libopus`, the QUIC/UDP, RTMP, V4L2, NVIDIA, VA-API, UniFFI, and LibAV forks ([vendor-sync](/quest/m3/vendor-sync/README.md) ports upstream), and JS's Zod, pako, and UUIDv7. `moq-gst` keeps its Rust 1.92 floor for GStreamer.

## Audit

- deps

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
