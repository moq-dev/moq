# [S] Expose streamed DEFLATE headers on open and add MoqMetaGroup::index and meta::Group::end

## Goal

Metadata consumers receive streamed DEFLATE frame headers as soon as each frame opens (rather than waiting for `finish()`), `MoqMetaGroup` in `moq-ffi` exposes `index()` matching Rust `meta::Group::index()` and JS `Meta.Group.index`, and `meta::Group::end()` is exposed in Rust and JS for indexers such as `moq-hls`.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ keep [moq-dev/moq#9793](https://github.com/moq-dev/moq/pull/9793)'s `broadcast.meta()` API shape for `0.22.0` and track these three additive follow-ups in `m3`.

Caused by [moq-dev/moq#9793](https://github.com/moq-dev/moq/pull/9793) (which accepted the streamed DEFLATE finding under `afk` and omitted `index()`/`end()`).

1. **Streamed DEFLATE headers (`rs/moq-net/src/model/meta.rs:157`):** expose a chunked compressed frame's declared header when the frame opens instead of waiting until `finish()` publishes its payload.
2. **`MoqMetaGroup::index()` (`rs/moq-ffi/src/consumer.rs:828-854` and language wrappers):** mirror Rust `meta::Group::index()` and JS `Meta.Group.index` so binding callers know which frame index to resume payload from.
3. **`meta::Group::end()` (`rs/moq-net/src/model/meta.rs`, `js/net/src/meta.ts`):** expose the group's declared end (previously omitted to satisfy dead-code checks before `moq-hls` switched to metadata subscriptions).

Tests: unit tests covering streamed DEFLATE header visibility on open, `meta::Group::end()` in Rust and JS, and `MoqMetaGroup::index()` in `moq-ffi`.

Public API: additive (`MoqMetaGroup::index`, `meta::Group::end`). Wire: none.

## Related

- [Upgrade moq_hls::Server with metadata indexing, idle eviction, scoping hooks, cold-edge lookup, and transmux coalescing](/quest/m3/hls-server-lifecycle.md) - primary consumer of `meta::Group::end()`
