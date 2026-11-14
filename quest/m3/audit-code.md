# [M] Code audit

## Goal

Code, exports, Cargo features, and CLI flags that nothing uses are deleted.
Code that costs the most complexity for what it delivers is simplified:
duplicate mechanisms merged, and layers, options, and generics with one user
collapsed. Every lint suppression in every language is still needed and says
why, and nothing calls an API deprecated at the workspace MSRV.

## Plan

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, with its own `Required`
naming the [release cut](/quest/m3/release/cut.md) and Check first rewritten. Fixes land
in that PR; only maintainer decisions (public API, scope) and changes too
large to review alongside become follow-ups.

Removing or reshaping a public item, narrowing a feature, or removing one of
our deprecated exports or hidden CLI spellings is the maintainer's call, so
each is a follow-up with a recommendation. The public-item pass before the m2 cut
ran as the api-shape epic's own work; this audit keeps private code.

Decided 2026-11-11 (maintainer, on [moq-dev/moq#8461](https://github.com/moq-dev/moq/pull/8461)):
✅ that sweep's kept hits are this audit's baselines (rejected: a new quest).
Vendored forks kept upstream's surface: moq-v4l 1,666 hits (mostly generated
`videodev2.rs`), moq-vaapi 707 (mostly generated `libva.rs`), moq-nvenc's
generated `sys`, moq-quic's `RetryError::into_incoming`, and moq-rtmp's `rml`.
moq-c kept its 11 entry points, which mirror moq-ffi for outside C callers.

Previous runs: dead code [moq-dev/moq#6166](https://github.com/moq-dev/moq/pull/6166),
simplification [moq-dev/moq#6527](https://github.com/moq-dev/moq/pull/6527),
lints [moq-dev/moq#6576](https://github.com/moq-dev/moq/pull/6576).

### Check first: dead code

- Rustc misses unused `pub` items. Tokenize the tree and shortlist `pub`
  declarations whose name appears only at the definition.
- Unconditional dead-code suppressions are `expect`, so a stale one fails the
  build; check what the `cfg_attr` ones still guard.
- Knip needs explicit entries for Bun tests, `*.browser.ts`, benches,
  examples, worklets, TSX, `test/*` clients, and repo-root scripts, and
  `ignore` for `rs/**/test_data`.
- Trace a CLI flag through the config merge, not prose or examples. Hidden
  deprecated spellings (`--client-*`, `--server-*`, the relay's legacy auth
  fields) fail with migration guidance on purpose.
- Windows clippy reports `capture::pump::Read::Idle` (only X11 idles) and
  `I420::from_bgra`, whose cfg still names Windows though only PipeWire calls
  it.

### Check first: simplification

- Still open from m0: `ietf/session.rs` `start()` builds both halves twice
  across the draft arms; lite and IETF `GroupServe` share their open, flush,
  and FIN-settle driver.
- The codec importers each forward `demand`, `flush`, `finish`, `abort`,
  `cut`, `discontinuity`, and `seek` to their container producer. Dropping
  them means a shared track handle, which is an API proposal.
- Then the largest moq-net modules and the bindings' forwarding layers.
  Search the quest tree before fixing a duplicate.
- Kept on purpose: traits with several implementations or device-free test
  doubles; ietf `RequestWaker` and lite `FetchWake` apart from the queue-once
  waker; js/net's hand-written subscriber grant checks; the separate lite and
  IETF JS datagram loops; the thin Dart, C++, and Swift wrappers; the
  one-line `until_wanted` loops over `Demand::wanted_changed`; js/net's lite
  version gates mirroring Rust's.

### Check first: lints

- Already gated, so skip: `clippy::allow_attributes_without_reason`,
  unfulfilled `expect`s, Biome's unused `biome-ignore`, ruff's `RUF100`, and
  pyright's unnecessary `type: ignore` in the Python wrapper. Review what
  those can't see. Generated and vendored bindings keep their blanket allows;
  `shellcheck disable` is out of scope.
- CI lints Linux only, so run clippy for `aarch64-apple-darwin` and
  `x86_64-pc-windows-gnu` from Linux with a rust-overlay toolchain and
  `zig cc` for build scripts. That found every stale suppression last run.
- Swift: read the nightly macOS job's Swift step for warnings. TypeScript:
  run typescript-eslint's `no-deprecated` once it supports the pinned
  TypeScript, and consider joining CI. The CARLA sim's pyright needs the
  bindings installed to check its ignores.
- `fetch_update` is deprecated on nightly in favor of `try_update`, stable
  only from Rust 1.95; keep it while the MSRV is 1.91.
- `rs lint`'s `git grep` for `set_var`/`remove_var` becomes a
  `disallowed-methods` entry in `clippy.toml` once crate- and module-wide test
  `expect(clippy::disallowed_methods)` move onto the items that read the
  clock (decided 2026-10-31, maintainer).
- AVFoundation's `devicesWithMediaType` stays: its replacement needs macOS 14.

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
