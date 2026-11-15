# [S] Cargo feature review before the cut: delta pass

## Goal

Every Cargo feature m2 added or changed since the first pass (2026-11-14) has
been judged before the release ships, by the same rules: defaults build
nothing expensive or system-dependent; each feature that remains is a vendor
integration or a system dependency and lives in the crate that owns that
integration, forwarded by dependents under the same name; a feature that only
trims compile time is removed in favor of always-compiled code. The scope
covers the defaults and `full` sets of shipped binaries, and the feature lists
the bindings and packaging builds pass. Each change the maintainer approves is
a quest that lands before the cut. A feature that matches a reason the Plan's
Kept list records (the poll-first `async` design, a test or harness hook, an
optional `serde`, or a layering split) also stays.

## Plan

Recurs once per API or wire cut (decided 2026-10-26; per breaking cut
2026-11-09). The [release cut](/quest/m3/release/cut.md) re-creates this quest
when the maintainer names the next API or wire cut, so the PR finishing this
run adds no successor. Record what this run learned in the cut's PR notes or a
follow-up quest. Only `/quest-delete` ends the recurrence.

Decided 2026-11-12 (maintainer, /quest-audit): a first pass on `main` as it
stood, then this delta pass after m2. Like the [API shape
review](/quest/m3/release/api-review.md), it changes no code: it proposes
each change with a recommendation and files each approved change as a quest
in m2.

What the first pass ([moq-dev/moq#8765](https://github.com/moq-dev/moq/pull/8765))
judged, so this pass reads only the difference:

- Method: list every `[features]` table (`git ls-files '*Cargo.toml'`), every
  internal dependency's `features =` and `default-features`, every
  `--features` in `.github/`, `nix/`, `.cargo/config.toml`, `bench/run.sh`,
  `rs/justfile`, the binding build hooks (`dart/`, `py/`, `pyproject.toml`,
  `cpp/`), and count `cfg(feature = ...)` sites per crate. Measure a
  feature's cost with `cargo tree -e normal --prefix none` with and without
  it, inside the shipped binary rather than the crate alone: most features
  that look heavy on their crate add one or two crates to moq-relay or moq-cli.
  Diff this pass's inventory against the first pass's to find what m2 added.
- Filed: [network stack](/quest/m2/features-network.md),
  [qlog](/quest/m2/features-qlog.md), and [media and gateway
  trims](/quest/m2/features-leaf-trims.md). Check each landed or was dropped.
- Kept, with the reason a new feature has to match:
  - `async` in kio, moq-net, hang, moq-json, moq-mux, moq-stats, moq-hls,
    moq-crypto, and moq-auth: the poll-first design (decided 2026-11-05 in
    [moq-dev/moq#7607](https://github.com/moq-dev/moq/pull/7607)), checked
    by `just rs coroutines`. kio's `runtime`, `macros`, `sim`, and `manual`
    follow it.
  - Vendor or system: moq-video's `nvidia`, `vaapi`, `mediacodec`, `v4l2`,
    `openh264` (a vendored C++ build), `vpx` (system libvpx), `pipewire`
    (libpipewire), and `render` (wgpu); moq-audio's `capture`, `playback`,
    `pipewire`, and `pulseaudio` (libasound through cpal); `jemalloc` (a C
    build); moq-cli's `s3`, `gcs`, and `azure`.
  - Large HTTP stacks a library embedder owns: moq-auth's `client`
    (reqwest) and `serve` (axum), moq-hls's and moq-webrtc's `server` (axum).
  - moq-crypto's `sign`, `e2ee`, `cpu`, and `provider`: they keep moq-net out
    of moq-quic's graph, a layering need rather than a trim.
  - `serde` in kio, moq-pattern, and moq-crypto: an optional dependency, by
    ecosystem convention.
  - moq-video's `capture`: device I/O the bindings deliberately leave to the
    language wrappers, and its platform frameworks would bloat mobile builds.
  - moq-cli's `capture`, `play`, and `transcode`: [Ship capture and
    playback](/quest/m3/cli-packaging.md) owns them and the `full` set.
  - Test and harness hooks, off by default and never in a shipped build:
    `fuzz` (moq-net, moq-mux, moq-quic, moq-rtmp, moq-sock, moq-uring),
    `sim` (moq-quic, moq-tokio), `test-support` (moq-relay, moq-rtsp),
    `testing` (moq-ffi), `test` (moq-shaper), `bench` (moq-json, moq-stats),
    `_manual` and `_probe` (moq-uring), and uniffi-bindgen's `cli`. Cross-crate
    tests and fuzz crates are separate packages, so a dev-dependency cannot
    replace them.
- Corrected premise: storage features belong in moq-cli, not moq-archive.
  moq-archive is generic over any `ObjectStore`; moq-cli's `open_store` is the
  code that integrates S3, GCS, and Azure.

What to check first in this pass:

- New `[features]` entries and new `default-features = false` consumers since
  the first pass, especially in crates m2's routes and cluster work touched
  (moq-net, moq-relay, moq-tokio).
- Whether [ALSA without a load-time
  link](/quest/m3/capture-alsa-link.md) landed: if libasound loads at run time,
  moq-audio's `capture` and `playback` no longer need a system library to
  build, and their premise needs re-judging.

## Required

- [m2](/quest/m2/README.md) - the delta pass judges what m2 added

## Related

- [Cost audit](/quest/m3/audit-cost.md) - owns per-crate build cost and which
  dependencies exist
