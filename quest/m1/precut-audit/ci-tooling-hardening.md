# [S] Harden API-diff deletion checks, fuzz exclusions, release workflow guards, and metadata/docs

## Goal

`sh/api-diff/run.sh` catches deleted or privatized JS packages, workspace
selection excludes every `*-fuzz` crate automatically, FFI/nightly workflows
reuse shared artifacts and stop on upstream build failure, and crate metadata
plus relay/dev docs are accurate for `0.22.0`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place these 4 non-cut-blocking CI/tooling/metadata
fixes in `m1` while the 4 cut-blocking release/packaging bugs gate the cut in
`m0` (rejected: putting all 9 in `m1`; dropping these 4).

1. **Catch deleted/privatized JS packages in `sh/api-diff/run.sh:231-242`.**
   - Currently iterates only HEAD's published JS packages, silently missing a
     package deleted or marked `"private": true` since baseline. Iterate the
     union of baseline and HEAD published JS packages so removals fail
     `--deny`.
2. **Exclude all `*-fuzz` crates in workspace selection (`sh/rs/select.sh:183,269`,
   `sh/rs/coverage.sh:8`, `sh/rs/select.test.py:13`).**
   - `moq-auth-fuzz` and `qmux-fuzz` are missing from the hardcoded exclusion
     list, pulling `libfuzzer-sys` into normal workspace runs. Derive `*-fuzz`
     exclusions from `cargo metadata` (or add the missing crates and test in
     `sh/rs/select.test.py`).
3. **Workflow cleanup and failure propagation:**
   - `.github/workflows/release-dart-ffi.yml:28-48`: reuse the 9 overlapping
     FFI library targets from `release-shared-ffi.yml` instead of rebuilding
     them.
   - `.github/workflows/release-py-ffi.yml:99-100`: remove the stale
     `yum install -y clang-devel` step if no longer needed after rind/tooling
     updates (verify wheel build).
   - `.github/workflows/release-cpp.yml:39-40` &
     `.github/workflows/nightly.yml:748-749,758-759`: stop downstream jobs from
     running when `native` or `release-shared-ffi` fails.
4. **Crate metadata and stale doc URLs:**
   - Add `rust-version.workspace = true` to `rs/moq-ffi/Cargo.toml` and add
     `keywords`/`categories` to `moq-auth`, `moq-congestion`, and `moq-gst`.
   - Fix stale URLs in `packaging/moq-relay/relay.toml:5,38`
     (`https://doc.moq.dev/bin/relay/auth`) and update `README.md:154` /
     `doc/setup/dev.md:20` to state accurately that Check runs on `release` PRs
     and the `main` merge queue.

Public API: none. Wire: none. Run `sh/rs/select.test.py`, `sh/api-diff` test
checks, and `just check`.
