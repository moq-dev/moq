# [M] Audit the code

## Goal

Run the `code` audit: `quest audit code` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-23 at `890ea4e531` (before the 0.22.0 cut). `just js dead-code` (`fallow` 3.44.0, 626 entries) passed with 0 issues; all 21 `fallow-ignore-next-line` sites were verified public API members. Previous runs: dead code [moq-dev/moq#6166](https://github.com/moq-dev/moq/pull/6166), simplification [moq-dev/moq#6527](https://github.com/moq-dev/moq/pull/6527), lints [moq-dev/moq#6576](https://github.com/moq-dev/moq/pull/6576).

### Excluded scope and kept baselines

- Decided 2026-11-11 (maintainer, on [moq-dev/moq#8461](https://github.com/moq-dev/moq/pull/8461)): vendored forks keep upstream's surface (`moq-v4l`, `moq-vaapi`, `moq-nvenc` generated `sys`, `moq-quic::RetryError::into_incoming`, `moq-rtmp::rml`, and `moq-congestion::gcc`'s ported WebRTC helpers). `libmoq` keeps its C entry points mirroring `moq-ffi` for outside C callers.
- Kept on purpose: traits with several implementations or device-free test doubles; IETF `RequestWaker` and lite `FetchWake` apart from the queue-once waker; `js/net`'s hand-written subscriber grant checks; separate lite and IETF JS datagram loops; thin Dart, C++, and Swift wrappers; one-line `until_wanted` loops over `Demand::wanted_changed`; `js/net`'s lite version gates mirroring Rust's; hidden deprecated CLI spellings (`--client-*`, `--server-*`, legacy relay auth fields) that fail with migration guidance.

### Check first

- Dead code: tokenize non-vendored Rust crates for `pub` declarations referenced only at their definition (remembering `#[kio::wrapper]` generates non-`poll_` async callers); review `cfg_attr(..., allow(dead_code))` suppressions (e.g. `DmaBuf::new` in `rs/moq-video/src/frame.rs:288`) and `fallow-ignore-next-line` suppressions in `js/`.
- Simplification: lite and IETF `GroupServe` duplication ([shared-serve](/quest/m4/shared-serve.md)); codec importers forwarding `demand`/`flush`/`finish`/`abort`/`cut`/`discontinuity`/`seek` (API proposal); largest `moq-net` modules and bindings forwarding layers.
- Lints: already gated (`allow_attributes_without_reason`, unfulfilled `expect`s across Linux/macOS/Windows, unused `biome-ignore`, ruff `RUF100`, pyright in `py/moq-rs`), so focus on what those miss: Swift warnings in Nightly macOS logs, `typescript-eslint` `no-deprecated`, CARLA pyright, moving crate-wide test `expect(clippy::disallowed_methods)` onto clock-reading items so `set_var`/`remove_var` can move into `clippy.toml` (decided 2026-10-31). Keep `fetch_update` while MSRV is 1.91 and AVFoundation `devicesWithMediaType` while macOS < 14 is supported.

## Audit

- code

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
