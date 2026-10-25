# [S] fMP4 passthrough starts at a sync sample

## Goal

An fMP4 passthrough video track whose import starts mid-GOP publishes nothing
until its first sync sample, as the unwrapped path already does, so no group
starts with frames a decoder cannot use.

## Plan

Decided with the maintainer on 2026-10-24: m1, a correctness bug in a shipped
import path.

- Found reviewing [moq-dev/moq#6686](https://github.com/moq-dev/moq/pull/6686):
  the unwrapped path skips to a sync sample, while passthrough publishes the
  leading non-sync fragments as the start of a group.
- #6686 adds `Fmp4Track::orphaned` to drop samples that depend on a sync
  sample its edit list dropped; reuse it for a join before the first sync
  sample rather than adding a second rule.
- Start after #6686 lands. Test both modes with a fragment that starts mid-GOP.
