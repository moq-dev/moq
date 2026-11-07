# [XS] Release's audio encoder rolls groups before the cap

## Goal

On `release`, `@moq/publish`'s audio encoder starts a new group before a group
reaches `MAX_GROUP_FRAMES` or `MAX_GROUP_CACHE_BYTES`, so a long
`groupDuration` with short frames never throws `GroupTooLarge`. It ships in
the next release as a patch.

## Plan

The fix is [moq-dev/moq#8118](https://github.com/moq-dev/moq/pull/8118), a PR
into `release` only: `Audio.Encoder` counts the open group's frames and bytes
and rolls at the net caps, tested with 2.5 ms frames and 1 MiB frames under a
60 s `groupDuration`. `main` has no group cap
([unbounded groups](/quest/m2/unbounded-groups/README.md)), so it gets no
change.

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ cut it now as 0.5.6, a
patch (rejected: waiting for a bigger cut).
[moq-dev/moq#8118](https://github.com/moq-dev/moq/pull/8118) merged into
`release` as c4c4078293, and
[moq-dev/moq#8213](https://github.com/moq-dev/moq/pull/8213) bumps
`@moq/publish` to 0.5.6 there; merging it publishes through `release-js.yml`.

Once the patch publishes, delete this quest.

Public API: none. Wire: none.

## Waiting

- Maintainer: merge [moq-dev/moq#8213](https://github.com/moq-dev/moq/pull/8213), the 0.5.6 bump on `release`; merging is the maintainer's pick. Check that `npm view @moq/publish version` prints 0.5.6.
