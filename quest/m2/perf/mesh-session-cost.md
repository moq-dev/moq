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
