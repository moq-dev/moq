# [M] Unify dependency features with cargo-hakari

## Goal

Every CI job, the queue gate, Nightly's platform jobs, and the FFI builds
compile each dependency with the same features, so mbx's cache hits across
them. A cargo-hakari workspace-hack crate pins the union of the dependency
features, and the gate fails when it is out of date. Record mbx's hit rate across
jobs, and their CPU time, before and after.

## Plan

Decided with the maintainer on 2026-10-25: use cargo-hakari, the standard tool
from guppy's authors, rather than only trimming CI's feature permutations. mbx
keys every compilation on its inputs, so a dependency built with a different
feature set in another job is a miss, however warm the cache.

- Generate the workspace-hack crate with `cargo hakari generate`, and add a
  `cargo hakari generate --diff` and `cargo hakari manage-deps --dry-run` check to
  `just check`.
- moq publishes crates with release-plz, and a published crate cannot depend on
  an unpublished workspace-hack. Use hakari's supported setup for published
  workspaces, and prove a release-plz dry run still publishes.
- Keep the feature-contract checks, which `just rs features` runs nightly and
  on release PRs, testing each crate's own declarations: the
  `quic-features` recipe in `rs/justfile` (moq-quic has no native runtime),
  `sh/rs/media-features.sh` (minimal builds exclude codecs and rendering), and
  `sh/rs/net-features.sh` and `sh/rs/tokio-features.sh`. A workspace-hack would
  inject the union into them and hide a missing declaration. Exclude the crates
  they guard from the hack, or run those checks with it disabled
  (`cargo hakari disable`), and show each still fails on a planted violation.
- Unifying features means some packages build features they don't use. Measure
  whether that costs more CPU than the cache hits save; target, profile, and
  `RUSTFLAGS` differences still split keys.
- Measure the baseline here, before generating the hack: across jobs, which
  compilations one job misses although another job built the same crate at
  the same version, grouped by what differs (features, profile, `RUSTFLAGS`,
  target). Repeat it after, from the same commit.

Decided 2026-10-27 (maintainer): this quest no longer waits on
[Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md). It measures its own
before and after baseline, so the cross-job miss report moved here from that
quest, which stays a waiting quest for the R2 numbers alone.

Public API: none; the workspace-hack crate is not published.
