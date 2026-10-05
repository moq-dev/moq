# [M] DVR rewind

## Goal

A viewer seeks through a bounded archive and returns to live playback using the
same timeline and group-range objects as an unbounded archive.

## Plan

The recording writer owns retention, deletion grace, checkpoint recovery, and
restart cleanup. This quest consumes that contract and owns viewer seek and
return-to-live behavior, not a second writer implementation.

The player reads the archive timeline, FETCHes old groups through the normal
miss chain, and splices back to SUBSCRIBE at the live edge without opening a
second media format. Missing groups remain ordinary gaps.

An unbounded archive can continue the same segment numbering without rewriting
objects retained from an earlier DVR window.

Test seeks within the retained window, expiry during a seek, missing groups,
restart recovery, and return to live without duplicated or rewound playback.
Use the writer/reader fixtures; a retention defect is fixed in its owning layer.

The reader evicts popped spans from its object cache, but a group it already
served stays in `moq_net`'s track cache until the pool reclaims it. Decide
whether expiry during a seek needs a group eviction API in `moq-net`.

`moq-hls` reads a timeline from the catalog's own broadcast, and an
`archive.replay` path only marks it non-durable. Decided (09-29): the exporter
does not follow `replay`, and viewers don't address a separate replay
broadcast. A recording is the broadcast. Under the
[wildcard](/quest/m0/wildcard/README.md) plan the archive serves the source
path through the root claim, and a live announcement shadows it. So when live
ends, `moq-hls` resolves the same name and falls through to the recording:
playlists keep serving for rewind and for players finishing the last
segments. The fall-through needs the recording to publish its catalog live,
since `moq-hls` subscribes to it rather than FETCHing it. Decided (09-29):
that republishing moves into `moq-archive`, so any host of the archive
behind the root claim does it, not only `moq-cli`; update [Replay
catalog](/quest/m1/archive/replay-catalog.md) to match.

During live, rewind needs no handover: a recorded broadcast's live timeline
is durable, and every group it lists is promised available, so a seek past
the live window FETCHes old groups through the normal miss chain down to the
archive. `moq-hls` needs no special path. `moq-hls` gains
no linger (the `hls-linger` quest was dropped, because an unannounced
broadcast can't be FETCHed and a linger would only serve the cache). Test
the handover: a live HLS session keeps its playlist URIs and media sequence
numbers when the name moves from the live publisher to the archive.

## Required

- [Per-track timelines](/quest/m1/archive/track-timeline.md) - seeks through per-track timelines
- [Replay catalog](/quest/m1/archive/replay-catalog.md) - the recording publishes its catalog live, so `moq-hls` finds it after the handover
- [Wildcard](/quest/m0/wildcard/README.md) - the archive's root claim serves the source path once the live announcement ends

## Closes

- [#2275](https://github.com/moq-dev/moq/issues/2275) - close this issue when the quest finishes
