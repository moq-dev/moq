# [S] The bench comparison builds moq-gst's catalog bench on its base

## Goal

A paired bench comparison that selects `rust/gst/moq-gst/catalog` against
`origin/main` builds the base side and reports a comparison, instead of
marking it unavailable with "base build failed". Local reviews and gates
that run `quest` use the build `flake.nix` pins, or say which they ran.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7347](https://github.com/moq-dev/moq/pull/7347). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

- **Base build**: #7347's `just ci` bench comparison reported "base build
  failed" for `rust/gst/moq-gst/catalog` against `origin/main`, from a
  change that doesn't touch moq-gst. `bench/run.sh:396-404` builds the base
  in a detached `git worktree` under `$RUN/base` through
  `criterion_build_base` (`:330`), and any failure there marks every
  selected target unavailable without showing why. The bench target
  (`rs/moq-gst/Cargo.toml:47`, `benches/catalog.rs`) has existed since
  2026-10-14, so a missing target is not the cause. Check first whether
  it was the nested-checkout Cargo alias merge that
  [the paired bench baseline builds](/quest/m0/nightly/bench-baseline-build.md)
  found in Nightly: `$RUN` sits under `.tmp/bench` inside the checkout
  (`bench/run.sh:157`), and #7347's run predates
  [moq-dev/moq#7245](https://github.com/moq-dev/moq/pull/7245), which made
  those aliases strings. If a comparison on current main builds the base,
  only the logging below is left. Otherwise look for what the fresh
  worktree lacks that the checkout has (GStreamer's build environment, a
  generated or untracked input, submodules), reproducing with the bench
  command #7347's `just ci` ran, and fix the cause. Either way, have the
  base build log its error into the run's output.
- **quest version**: in the same run, the `quest` on the host's PATH was
  not the build `flake.nix` pins (`github:moq-dev/quest/1ef2ca7`, served by
  the dev shell); the review was rerun with the pinned one. Check which
  scripts or recipes call `quest` (the gate and `quest review` paths) and
  make them resolve the pinned build, or fail with a message naming both
  versions when they differ. Settle on whichever is simpler; the quest
  binary's own behavior is out of scope.

Public API: none. Wire: none.

## Related

- [The paired bench baseline builds](/quest/m0/nightly/bench-baseline-build.md) - the same "base build failed" in Nightly, from a nested checkout's Cargo aliases
