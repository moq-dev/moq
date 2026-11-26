# [M] Upgrade moq_hls::Server with metadata indexing, idle eviction, scoping hooks, cold-edge lookup, and transmux coalescing

## Goal

`moq-hls` indexes live renditions over metadata-only subscriptions so zero
media payload bytes cross the relay mesh until an HTTP client requests a
segment; `moq_hls::Server` supports idle eviction and per-request embedder
scoping/hooks so downstream gateways delegate to `Server` instead of
duplicating broadcaster pools; cold edges wait or fetch on demand instead of
returning spurious 404s before `WatchMedia` finishes indexing; and concurrent
requests for the same segment share one fetch and transmux pass.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS audit):

- ✅ Upgrade `moq_hls::Server` with idle eviction and scoping/hooks so
  embedders delegate to `Server` (rejected: keeping a parallel custom pool in
  embedders and exposing `include`/`identity` checks on `Broadcaster`).
- ✅ Fix cold-edge segment 404s (rejected: relying on player retries).
- ✅ Switch `WatchMedia` to metadata-only subscriptions now that
  [subscribe-meta.md](/quest/m2/subscribe-meta.md) lands in `m2`.

Scope in `rs/moq-hls`:

1. **Metadata-only live indexing (`src/export/rendition.rs`):**
   - Switch `WatchMedia` to a metadata-only subscription on timed lite-07
     tracks where wire headers and catalog updates supply everything the
     watcher needs, so opening `master.m3u8` or watching one rung avoids
     pulling full media payloads for unwatched renditions across the relay
     mesh.
   - Account explicitly for every payload-derived fact `WatchMedia` currently
     decodes so nothing regresses:
     - **Sample PTS & `Reach` (`rendition.rs:2552-2558, 2638-2640`):** on timed
       tracks, use wire frame timestamps and `GROUP_END` (`group.end()`) for
       group start/end and reach; keep a payload subscription on scale-less or
       legacy (`lite-06` / `moq-transport`) tracks that carry no wire
       timestamps.
     - **Discontinuity markers (`rendition.rs:2472, 2646`):** a container
       discontinuity frame can be non-zero bytes on the wire and empty only
       after container decoding; handle via a targeted frame-0 payload probe
       (or container-level empty-group signal) so pauses never become media
       segments.
     - **Resized video hand-over:** when a resized video group arrives before
       its catalog update, `WatchMedia` currently inspects keyframe picture
       dimensions to assign the group to the new init segment. Retain a
       targeted frame-0 payload probe (or payload probing on inline-parameter-set
       / resizable video renditions where headers and catalog data do not
       suffice) so a resized group is never listed under the old init segment.
2. **Idle eviction and embedder hooks on `moq_hls::Server` (`src/server/`):**
   - Add configurable idle eviction to `Server`'s broadcaster pool so
     long-lived streams tear down subscriptions when no HTTP requests arrive.
   - Expose per-request scoping/hooks on `Server` (tenant/origin scoping,
     live-only mode without lingering tail, rejecting `Minted` identities,
     auth/error mapping, packaging/query stamping) so embedders get `include`
     routing and hashed `Identity` validation on `Media`, `Init`, and `Segment`
     requests without reimplementing pooling.
3. **Cold-edge segment lookup (`src/export/rendition.rs`, `src/server/routes.rs`):**
   - When `rendition.segment(&id)` misses on a newly opened broadcaster (where
     `broadcaster.ready()` fired as soon as another rendition, typically audio,
     listed one segment), wait boundedly for that rendition's `WatchMedia` to
     index the requested segment (or fetch on demand by grid time via bounded
     `SUBSCRIBE`) instead of returning an immediate synchronous 404.
4. **Segment transmux coalescing (`src/export/rendition.rs`):**
   - Coalesce concurrent `Rendition::segment()` requests for the same
     `(rendition, tag, segment)` so N concurrent viewers or cache PoPs trigger
     one group fetch and one fMP4 transmux pass.

Public API: additive `moq_hls::Server` eviction/hook configuration and
request-routing entry points. Wire: none beyond
[subscribe-meta.md](/quest/m2/subscribe-meta.md) and
[identity-rename-and-cap.md](/quest/m2/identity-rename-and-cap.md).

## Required

- [Rename Instance to Identity and cap it at 256 UTF-8 bytes on lite-07](/quest/m2/identity-rename-and-cap.md) - hashed `Identity` URL tokens that `Server` routes and validates
- [Metadata-only subscriptions](/quest/m2/subscribe-meta.md) - metadata-only subscriber API used by `WatchMedia`
