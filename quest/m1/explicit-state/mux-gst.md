# [M] TS and fMP4 export lifecycles

## Goal

The TS and fMP4 export lifecycles in moq-mux are explicit state: the TS
export program and its tracks, the TS follower, passthrough, and group
buffer, and the fMP4 export track. Each phase's data lives in its variant,
and no `.expect()` or flag ordering stands in for a phase.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

No confirmed bug is folded into this child. Two suspected ones were checked
and are not defects:

- An fMP4 export track that ends before it is described, while the catalog
  stays live, keeps `init_ready()` false. That wait is bounded: once other
  tracks have media it fails at the init queue limit, naming the track.
  Otherwise the live catalog can still move the rendition.
- A withdrawn TS track whose parked frames already went out stays listed.
  This is intended, and `revision_test.rs` covers it.

Instances to redesign:

- **TS export `Program`** (`rs/moq-mux/src/container/ts/export.rs`):
  `catalog`, `catalog_ended`, `cataloged`, `retired`, `left`, `psi`, and
  others. `resume` hand-resets six fields.
- **TS export track**: `finished` carries five meanings, and the "gone"
  predicate ANDs eight conditions.
- **TS `Follower.refused`** (`ts/follower.rs`) belongs inside `Serving`. The
  TS `Passthrough` start and clock, and the `GroupBuffer` content kind, also
  become explicit.
- **fMP4 export track** (`rs/moq-mux/src/container/fmp4/export.rs`):
  `source`, `described`, `finished`, `resume`, and `group_finished` encode
  its phases. Land after
  [moq-dev/moq#7916](https://github.com/moq-dev/moq/pull/7916), which
  rewrites this file.
