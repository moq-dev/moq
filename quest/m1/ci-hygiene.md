# [XS] CI runs don't cancel each other or cache a broken build

## Goal

A push to one PR never cancels another PR's run, and a failed run never
saves the Rust cache later runs restore.

## Plan

- Concurrency: `check.yml`, `obs.yml`, `android.yml`, `wasm.yml`, and
  `interop.yml` group by `${{ github.ref }}` only. Key them by event and PR
  number the way `platform.yml` does (#4370).
- Cache: `interop.yml` and `swift.yml` set Swatinem's
  `cache-on-failure: true`; #4384's interop cache got saved broken that way
  (`failed to run custom build command for aws-lc-rs`). Drop it. Check
  whether the shared `.github/actions/rust-cache` saves on failure, and make
  it save only on success.

Public API: none. Wire: none.

## Related

- [Merge queue](/quest/m1/merge-queue.md) - also touches the concurrency groups for queue refs
