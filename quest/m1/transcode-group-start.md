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

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - a double claim's two workers at one path are settled by interchangeable output
