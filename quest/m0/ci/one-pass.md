# [S] One CI pass

## Goal

The merge queue's Linux gate is one job that runs `just ci` once: one
checkout, one dev shell, one cache restore. It costs about what Test costs
alone today, because the work Check did beyond Test now runs nightly and on
release PRs.

## Plan

Why Check and Test are separate today (measured 2026-10-19): Test's build
already doubles as clippy (`RUSTC_WORKSPACE_WRAPPER=clippy-driver`), so
default features compile once for clippy and tests. Check's time goes to
compiles that cannot share with Test, because each feature set or profile is
its own artifact:

- feature permutations: tokio-features (206 s on a cache hit, including the
  `--all-features` clippy at 128 to 157 s), media-features (95 s) and
  net-features (48 s);
- dart's `cargo build --release -p moq-ffi` (fat LTO), 6 to 10 minutes in the
  Check jobs that select it;
- docs, wasm clippy, uring-check, the `wasm-release` build, and the py and kt
  binding builds.

The `cache.yml` warm ran both halves in one job, and its Test step took as
long as a standalone Test, so folding them as they are would only add the
two times together.

Decisions (maintainer, 2026-10-19): shrink Check, then fold it.

- Move the feature permutations, the media variants and the release-profile
  binding builds to nightly and to PRs into `release`. A feature-gating bug
  then surfaces within a day instead of in the queue.
- Check the bindings with dev-profile builds where the queue still needs
  them.
- What stays (fmt and the other non-cargo lints, docs, wasm and uring clippy)
  runs as a preamble in the same job as the tests. The `ci check` and
  `ci test` halves go away and `just ci` runs one dispatch.
- Rejected: a hermetic per-crate Nix build (crane) with content-addressed
  test results. It's the larger win, but an [XL] epic that would also
  replace mbx; revisit it after the audit.
- Rejected: keeping two jobs and only trimming each.

The single job keeps the name `Check`, so a required context never goes
missing: the maintainer removes `Test` from the main ruleset just before
this PR is enqueued, which only loosens the gate, and merge groups built
earlier still report both. [Release gates](/quest/m0/ci/release-gates.md)
owns the ruleset edits. Record the queue's wall time per entry before and
after in the PR.
