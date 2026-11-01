# [M] Mesh session cost

## Goal

moq-net's per-session cost in the random-mesh tests drops, measured by a
benchmark swept over sessions and announces: announce handling,
`sync_route`/`requests_route`, `OriginScope::relative` rebuilding its
patterns on every call, and per-session setup (about 25%).

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile of the moq-net mesh tests (8.4 s, barely changed there because the
cost is production code). Cache `OriginScope::relative`'s patterns first;
benchmark before changing the rest.

Public API: none expected.

Decided 2026-11-01 (maintainer, `/quest-plan`): the per-reader cost of a
delivered group belongs here, beside the rest of the per-session cost.
Measured by [moq-dev/moq#7263](https://github.com/moq-dev/moq/pull/7263):
a track read that does get a group still costs about 12k to 26k user-space
instructions per reader per group (1 to 16 readers per track). Part of it is
copying the large `group::Consumer` value, inside
`Poll<Result<Option<group::Consumer>>>`, through each layer that returns it;
consider boxing parts of `group::Consumer`, and measure it on the same read.
