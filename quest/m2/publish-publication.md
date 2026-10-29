# [M] @moq/publish owns rendition identity in one publication

## Goal

`@moq/publish` keeps each network broadcast's renditions in one publication
object mapping catalog id to kind, name, track, and owner, so renames,
replacements, and resets are structural instead of kept in agreement across
five maps and per-encoder state.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only.

Rendition identity fixes keep landing in `js/publish` (e24828a601,
fe311b3d34, 2938e78f77, 3791aecce3, 6e06a956b0, 644789413d, b32b7a7421,
d0337260a6). The state is spread over five name-keyed maps in `Broadcast`
(`#kinds`, `#names`, `#replacements`, `#served`, `#listed`), plus a
per-Broadcast `#advertised` WeakMap and three failure layers in each
encoder. Replace them with one publication object per network broadcast,
mirroring Rust's identity rules (`hang::catalog::VideoConfig::same_identity`
and `AudioConfig::same_identity`, enforced by `moq-mux` since
[moq-dev/moq#7110](https://github.com/moq-dev/moq/pull/7110)).

Public API: none. Wire: none.
