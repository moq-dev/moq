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
  change that doesn't touch moq-gst. `bench/run.sh:375-382` builds the base
  in a detached `git worktree` under `$RUN/base` through
  `criterion_build_base` (`:308`), and any failure there marks every
  selected target unavailable without showing why. The bench target
  (`rs/moq-gst/Cargo.toml:47`, `benches/catalog.rs`) has existed since
  2026-10-14, so a missing target is not the cause; look for what the
  fresh worktree lacks that the checkout has (GStreamer's build
  environment, a generated or untracked input, submodules). Reproduce with
  the bench command #7347's `just ci` ran, fix the cause, and have the base
  build log its error into the run's output.
- **quest version**: in the same run, the `quest` on the host's PATH was
  not the build `flake.nix` pins (`github:moq-dev/quest/1ef2ca7`, served by
  the dev shell); the review was rerun with the pinned one. Check which
  scripts or recipes call `quest` (the gate and `quest review` paths) and
  make them resolve the pinned build, or fail with a message naming both
  versions when they differ. Settle on whichever is simpler; the quest
  binary's own behavior is out of scope.

Public API: none. Wire: none.

## Related

- [Paired bench regressions bisected](/quest/m0/nightly/bench-regressions.md) - other paired-comparison work
