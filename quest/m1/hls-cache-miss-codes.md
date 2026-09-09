# [M] moq-hls answers 404 for cache misses and gone publishers

## Goal

`moq-hls` answers 404 for a segment the relay cannot serve because the group
is not cached (not found, old, evicted) or because its publisher is gone, and
500 only for a genuine failure. On a moq-lite upstream the classification
comes from registered stream codes decoded by `moq-net`, so it cannot drift
again. An IETF upstream cannot say a miss on a stream reset and still answers
500 there.

## Plan

### Cache misses

`is_cache_miss` (`rs/moq-hls/src/export/rendition.rs:591-596`) compares the
wire code against `moq_net::Error::to_code()`, the crate's legacy table
(`rs/moq-net/src/error.rs:422-459`: `Old` 2, `NotFound` 13, `Evicted` 31). No
reset has carried that table since `StreamError` replaced it. A miss now goes
out as 0x20, 0x22, or 0x23 (`error.rs:210-213`), values in moq-lite's
non-interpretable 32-47 range (`drafts/draft-lcurley-moq-lite.md:263-265`), so
the receiver decodes them to `StreamError::Unknown` and surfaces
`Error::Remote(32|34|35)` (`error.rs:231-246`, `:529-530`). An IETF upstream
collapses all three to INTERNAL_ERROR and back to `Remote(0)`
(`rs/moq-net/src/ietf/error.rs:80-92`, `:105`). Nothing matches, so every miss
that crossed a session, which in a relay is all of them, answers 500
(`rs/moq-hls/src/server/routes.rs:261-264`).

The legacy literals collide with IETF request errors instead: an unregistered
request code such as 0xD or 0x1F decodes to `Remote(code)`
(`ietf/error.rs:286`) and matches `NotFound` (13) or `Evicted` (31). A peer's
DELIVERY_TIMEOUT (0x2) does not collide: it decodes to `Error::Timeout`, whose
legacy code is 3, not `Old`'s 2. #3531 already decodes DOES_NOT_EXIST to
`Error::NotFound` (`ietf/error.rs:282`).

The tests build the remote shape from the same stale table
(`rendition.rs:614-637`, `Error::Remote(local.to_code())`), so they agree with
the code rather than the wire.

The work:

- Register NOT_FOUND, OLD, and EVICTED (or one CACHE_MISS) in moq-lite's own
  48-63 range (`draft-lcurley-moq-lite.md:267-268`; the stream table at
  `:292-312` assigns only NO_CAPACITY 0x30 there). Encode and decode them in
  `StreamError` so a received one is the named variant, not `Unknown`, and
  extend `stream_codes_round_trip` (`error.rs:691`). Mirror in
  `js/net/src/error.ts` `StreamCode` (:88-92 still carries the 0x20 values)
  and in `js/net/src/ietf/error.ts`.
- `is_cache_miss` matches variants (`NotFound`, `Old`, `Evicted`) and nothing
  by code. Rebuild the tests from the wire registry (`StreamError::to_code`,
  `ietf::error::to_stream_code`) so they fail when the two drift.
- IETF upstreams keep answering 500 for a stream-reset miss; that registry has
  no value for one. Say so in the doc comment.

### Gone publishers

A publisher that disconnects resets the fetch with code 0, which arrives as
`Error::Remote(0)` on dev (`error.rs:233`, `:529`) and as `Error::Cancel` on
main. Observed against a local relay: with a rendition bound to a publisher
that then disconnected, every segment still in the playlist window answered
500 (`hls request failed err=moq: remote error: code=0`) until it aged out.
The playlist keeps listing them because the timeline it renders from is a
different track, often a different broadcast, from the media
(`rs/moq-mux/src/source.rs:189-199`).

500 tells a CDN or player to retry a segment that can never be served; 404
says it is gone. Name that condition beside the misses rather than inverting
the test into "is this retryable", which the root guide forbids. Reachable on
both rendition shapes (the catalog's own broadcast and a named sibling), since
both hold the broadcast the catalog was read from.

Land both with regressions: a miss that crossed a session, and a bound
rendition whose publisher disconnected, each answer through
`routes.rs:255-259`.
