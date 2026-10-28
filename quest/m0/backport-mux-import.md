# [S] Publish the MKV, FLV, and VP9 import backport

## Goal

Condition: [moq-dev/moq#6974](https://github.com/moq-dev/moq/pull/6974)
has merged into `release`, and a moq-mux patch containing it has published.
The published muxer refuses malformed MKV/FLV input and preserves VP9
keyframe dimensions through 65536.

## Plan

The release implementation is ready for review in
[moq-dev/moq#6974](https://github.com/moq-dev/moq/pull/6974), backporting
[moq-dev/moq#6908](https://github.com/moq-dev/moq/pull/6908) on branch
`codex/backport-mux-import` without changing the public Rust API or MoQ wire.

Both release CI halves and all 1007 mux tests pass. Patch semver checking
passes 223 checks. The release changelog calls out that unsupported MKV
audio/video and FLV codecs now fail import instead of being dropped.

Remaining work is the authorized merge and patch publication. Check that the
published `moq-mux-v*` tag contains the backport's commit and that its release
workflow succeeded, then remove this quest. The maintainer requested no
merges or releases during the current PR-production sweep.

Public API: none. Wire: none.
