# [XS] Nightly size report

## Goal

A nightly job reports the size of what we ship, built the way the release
builds it, and alerts when any of it grows. Size regressions show
up within a day instead of in an app store review or a user's bundle
analyzer.

The job is implemented (`sh/size/`, the `sizes` job in `nightly.yml`).
What remains is the first nightly run on `main` after it merges, because
nightly is never dispatched on a branch:

- The `sizes` job succeeds and uploads `nightly-size-history`. It downloads
  its own run's release artifacts while the run is still in progress, which
  no local or PR run exercises.
- Compare its `sizes.json` with run 38143901976, the last nightly before the
  unified release profile (#5274), measured by `sh/size/release.py`
  (`download`, `sizes`, `seconds`). Record the relay, CLI and moq-c sizes and
  `Build` step times in the PR that deletes this quest. The Go FFI
  before/after is already recorded below.

## Plan

Decided in planning:

- Nightly, not per PR. Size moves only on dependency bumps, feature flips, or
  profile changes, so a per-PR comment would say "no change" almost every
  time.
- No hard budgets in CI. The job keeps a rolling history and fails when an
  artifact grows past a threshold against the previous run (5%) or the
  newest report at least 30 days old (10%). The window catches slow drift,
  which night-to-night checks miss. The existing `alert.yml` path turns the
  failure into a Discord alert.
- Measure the release jobs' own uploads instead of rebuilding: relay, CLI and
  moq-c archives, the Go mirror's FFI staticlibs, wheels, AAR/JAR and the
  XCFramework, for every target nightly builds (Linux x86_64 and aarch64,
  macOS, Windows). Bare binaries, `.deb` and `.rpm` repeat the archives.
- Build locally only what no release job makes: moq-ffi with
  `--no-default-features`, and the JS entries (`@moq/net`,
  `@moq/watch/element` with and without `/ui`, `@moq/publish/element`),
  bundled and minified from the built packages, first-load chunks only, raw,
  gzip and brotli.
- Keys drop release versions so a bump keeps the history comparable. A
  changed artifact set is refused until someone starts a fresh history.
- Record each release job's `Build` step seconds and show them in the
  summary, but never alert on them: hosted build time is too noisy.
- The Go mirror's 100 MiB file limit is already enforced by the Go FFI
  package step in nightly; the report tracks the same staticlibs.
- The job summary carries `cargo bloat --crates` for the ffi build from the
  symbol-preserving `profiling` profile, and retains the bundle metafiles.
- Out of scope: moq-wasm, which [Remove moq-wasm](/quest/m2/rs2ts-remove-wasm.md)
  deletes, Docker images, and per-target release assets nightly doesn't
  build.

Go FFI before and after #5274 (Release Go FFI in nightly run 38143901976
against the same workflow on #5274's final head, run 38168487160; one sample
each):

| Target | Staticlib | Cdylib | `Build` step |
|---|---:|---:|---:|
| x86_64-unknown-linux-gnu | 68.1 to 59.6 MB (-12.4%) | 31.2 to 28.6 MB (-8.2%) | 347 to 575 s |
| aarch64-unknown-linux-gnu | 66.8 to 58.5 MB (-12.5%) | 28.3 to 25.8 MB (-8.9%) | 383 to 397 s |
| aarch64-apple-darwin | 43.9 to 39.2 MB (-10.7%) | 23.9 to 22.2 MB (-7.0%) | 597 to 807 s |
| x86_64-pc-windows-msvc | 59.0 to 50.2 MB (-14.9%) | 22.4 to 21.4 MB (-4.4%) | 1002 to 830 s |

The largest Go staticlib (59.6 MB) leaves about 43% headroom under 100 MiB.

Considered and declined (do not re-ask):

- Rebuilding the native artifacts in the size job, one runner per
  architecture. The release jobs already build them for every target.
- Shrinking npm install size by dropping sourcemaps or `inlineSources`. Maps
  are 60-77% of the unpacked packages, but they never reach a browser and
  they give consumers real stack traces.
- Splitting the lite and IETF implementations out of `@moq/net`: version
  negotiation needs both at connect time.
- Feature gates in moq-tokio for reqwest, tracing-subscriber, usage-rs/toml,
  and tokio "full" in the bindings.
- Replacing the AV1 codec-string regex in hang. tracing-subscriber's env
  filter keeps regex linked anyway.

## Related

- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - the same nightly-trend shape for Criterion
