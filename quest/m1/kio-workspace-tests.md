# [S] kio's log tests pass in a workspace build

## Goal

kio's log eviction tests, including `eviction_frees_whatever_readers_do`,
pass in `cargo test --workspace` as they do in `cargo test -p kio`. If the
workspace build exposes a real eviction bug, the code is fixed, not the
test.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7161](https://github.com/moq-dev/moq/pull/7161), whose agent saw
two kio log tests fail only in a `--workspace` run.

Checked 2026-10-29: `cargo test -p kio --lib` passes with and without the
`_sim` feature, so the trigger is another dependency feature the workspace
unifies, such as one of `crossbeam-epoch`'s (kio frees evicted segments
through epoch reclamation). The cargo-hakari workspace-hack in
[moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985) excludes kio, so
it will not fix this.

Find the feature that flips the result, then decide whether the test assumed
something the code doesn't promise (such as when a deferred free runs) or
whether eviction is wrong under that feature. Prove the fix by running the
tests under the unified feature set.

Public API: none. Wire: none.
