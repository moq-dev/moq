# [S] moq-hls keeps an ended broadcast for its playlist window

## Goal

A player polling `moq_hls::Server` can finish the last segments of a broadcast
that just ended, and a republish of the same name takes over cleanly. Today
`Server` evicts a broadcaster as soon as its broadcast closes, so the playlist
404s mid-window. The moq.pro edge works around it with its own pool on top of
`Broadcaster` that holds an ended broadcast for the playlist window plus a
grace period and checks for a republish
([#3964](https://github.com/moq-dev/moq/pull/3964)). Once moq-hls owns this,
the edge deletes its pool.

## Plan

Decided: moq-hls owns the retention through an `export::Config` field (the
playlist window plus a grace period), so every embedder gets it and the edge
has nothing to duplicate. `Config` is `#[non_exhaustive]`, so the field is
additive on `main`.

Guidance:

- The eviction lives in `server/mod.rs` (`evict_closed` and the
  `is_closed` checks in `Server::broadcaster`). An ended broadcaster keeps
  serving its final playlist, marked ended if the renditions know it, until
  the linger expires.
- A republish during the linger replaces the ended broadcaster. Decide
  whether a request for the name resolves the new broadcast first and falls
  back to the ended one only when nothing is announced; match what the edge
  pool does today.
- Segments are fetched from the relay cache on request, so the linger is only
  useful within the cache's retention; say so on the field, as `window` does.
- A zero linger keeps today's behavior. Pick the default with the edge's
  current value in mind.
- Tests: a request inside the linger after close still gets the playlist and
  a cached segment; after it expires the name 404s; a republish inside the
  linger is served.
