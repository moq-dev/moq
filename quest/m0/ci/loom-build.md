# [S] Loom tests compile, and the gate proves it

## Goal

`just rs loom` builds and runs on `main`, and a PR that breaks the `--cfg loom`
build fails its own gate instead of the next Nightly.

## Plan

Found 2026-10-25: `RUSTFLAGS="--cfg loom" cargo test -p moq-net --test loom
--no-run` fails on main with E0599 at `rs/moq-net/tests/loom.rs:621`:
`Subscription::with_groups` was replaced by `with_range` in
[moq-dev/moq#6138](https://github.com/moq-dev/moq/pull/6138), which missed the
loom test because nothing on the PR gate builds with `--cfg loom`.

- Port the call to `with_range`, then run the full `just rs loom`.
- Add a compile-only `--cfg loom` step (`--no-run`) to the changed-Rust check
  when `kio` or `moq-net` changes, so the build break fails the PR.
- The Loom models that exceed their branch limit or run past the job timeout
  are tracked in [CI audit](/quest/m2/audit-ci.md); this quest only restores
  the build.

Public API: none. Wire: none.
