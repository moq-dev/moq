# [S] JS SUBSCRIBE_TRACKS listings follow stream credit

## Goal

A `@moq/net` draft-18+ SUBSCRIBE_TRACKS listing holds state in proportion to
what the peer reads, not to the catalog, as moq-net's does. Each listing
PUBLISH opens only as the peer's stream credit admits it, a FORWARD=1 listing
reads its next track only as its skips go out, and the listing no longer uses
the `Limits` request cap.

## Plan

`#listBroadcast` in `js/net/src/ietf/publisher.ts` starts a PUBLISH for every
listed track up to the `Limits` cap and skips the rest, and queues every
FORWARD=1 skip at once. Mirror Rust's `run_listed_broadcast` in
`rs/moq-net/src/ietf/publisher/tracks.rs`: stage one track per broadcast, open
its stream only as credit admits, keep reading the listing for removals
meanwhile, since an unlisted track's PUBLISH_DONE is what returns credit, and
skip a track before its stream opens. Stream credit bounds open streams only
once `@moq/qmux` returns it when both halves close
([moq-dev/moq#6644](https://github.com/moq-dev/moq/pull/6644)).

Test with a peer that withholds credit and reads nothing, swept over catalog
size.

Public API: none expected. Wire: none.
