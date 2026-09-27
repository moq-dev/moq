# [M] Nightly size report

## Goal

A nightly job reports the size of what we ship, built the way the release
builds it, and alerts when any of it grows. Size regressions show
up within a day instead of in an app store review or a user's bundle
analyzer.

## Plan

Decided in planning:

- Nightly, not per PR. Size moves only on dependency bumps, feature flips, or
  profile changes, so a per-PR comment would say "no change" almost every
  time.
- No hard budgets in CI. The job keeps a rolling history and fails when an
  artifact grows past a threshold against the previous run (start at 5%) or
  against a longer window (for example 10% over 30 days). The window catches
  slow drift, which night-to-night checks miss. The existing `alert.yml` path
  turns the failure into a Discord alert. Add the workflow to its list if
  needed.
- Scope:
  - moq-ffi, default and `--no-default-features`, as cdylib and staticlib
  - libmoq, moq-relay, and moq-cli
  - the moq-wasm module, raw, gzip, and brotli
  - the consumer cost of the JS entries: `@moq/net`, `@moq/watch/element`
    with and without `/ui`, and `@moq/publish/element`. Measure them the way
    a consumer sees them: bundled and minified from the built packages,
    first-load chunks only, gzip and brotli.
  - the packaged outputs nightly already builds. `nightly.yml` runs the
    Python, Kotlin, and Swift release builds, so read their wheel, AAR, and
    xcframework sizes instead of rebuilding them.
- Out of scope: Docker images, and per-target release assets nightly doesn't
  build. The Docker quest measures its images once. Add others only if one of
  them regresses unnoticed.
- The job summary also carries `cargo bloat --crates` for the ffi build and a
  metafile breakdown for the publish element, so the cause of a jump is
  visible without a local rebuild. `cargo bloat` needs symbols, so build that
  pass with `CARGO_PROFILE_RELEASE_STRIP=none` and report stripped sizes
  separately.
- Following the tooling questline, the work lives in a `sh/` script behind a
  `just` recipe, and `nightly.yml` calls the recipe.

Considered and declined (do not re-ask):

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

- [Release profile](/quest/m1/release-profile.md) - the profile this report measures
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - the same nightly-trend shape for Criterion
