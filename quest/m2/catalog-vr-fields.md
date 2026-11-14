# [S] Decide whether the VR catalog fields ship

## Goal

No hang catalog field ships at the cut without a reader: the VR projection,
stereo, and ambisonic fields either gain a consumer or leave hang (Rust and
JS), the catalog draft, and the importers that fill them.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
[moq-dev/moq#5543](https://github.com/moq-dev/moq/pull/5543) added the fields
(`rs/hang/src/catalog/video/projection.rs`) and
[moq-dev/moq#7308](https://github.com/moq-dev/moq/pull/7308) fills them on
import (`rs/moq-mux/src/codec/spatial.rs`). `@moq/watch` reads none of them,
their one consumer quest (watch-webxr) was pruned in
[moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423), and `release`
has none, so the cut would freeze catalog JSON nobody reads.

Public API: (a) removes the fields from `hang` and `@moq/hang`, unpublished.
Wire: (a) removes them from the unpublished catalog JSON.

## Waiting

- Maintainer: (a) delete them before the cut (recommended: git keeps them for a WebXR player); (b) keep them and name a consumer, restoring a WebXR player quest
