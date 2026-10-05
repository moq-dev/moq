# [S] Transcoders start at group boundaries

## Goal

Two moq-transcode instances serving one derived path are interchangeable at
group boundaries, so a relay moving a subscription between them never splices
two encoders' output mid-group. moq-transcode (`rs/moq-transcode`, and
`moq transcode` in moq-cli) resolves a subscription that starts at (N, M>0)
to (N+1, 0), and refuses a FETCH that starts mid-group. Output groups mirror
the source's sequence numbers, and the catalog derives only from the input and
config.

## Plan

- The lite draft already lets a publisher skip a group it cannot serve from
  the requested frame and resolve to a later one, so no wire change.
- Output groups already mirror the source's sequences (`rung.rs`). Keep that,
  and check that nothing per-process (start time, random ids, encoder
  defaults) leaks into the catalog.
- Tests: a subscription and a FETCH that start mid-group, and two instances
  fed the same source that publish the same catalog and group sequences.

Public API: none. Wire: none.

### Findings

Done: the rung's fetch handler refuses a mid-group FETCH (it used to write
the whole group at the requested index), and a test pins two instances
publishing the same catalog and groups from one source.

Open, and not reachable from moq-transcode alone:

- moq-net serves subscriptions and cached fetches from the track cache, so a
  mid-group start is positioned into whatever group N the transcoder holds
  (`position_group` in `lite/publisher.rs`, the cache path of
  `fetch_group`). The handler only sees cache misses.
- The relay does not ask the new route for (N, M) anyway. `resume.rs`
  subscribes the new copy from (N, 0) and continues the in-flight group from
  that copy's group N at frame M, which is exactly the splice. The draft
  and `resume.rs` assume every route's frames are identical; two encoders'
  are only group-aligned.
- Rung names are minted per process (`video/120p.2` after a resize, see
  `catalog::Names`), so an instance that started after a resize names the
  same picture differently, and reuses a name another instance finished.
- With `encode::Kind::Auto` the probed catalog entry (profile, level) depends
  on which backend the host has.

Which way to close the splice is a maintainer decision: a track property that
limits route moves to group boundaries, relays moving routes only at group
boundaries, or giving each worker its own path.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - a double claim's two workers at one path are settled by interchangeable output
