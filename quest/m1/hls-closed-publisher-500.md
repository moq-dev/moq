# [S] moq-hls: a segment whose publisher is gone answers 500, not 404

## Goal

An HLS/DASH segment whose media broadcast has closed answers `404`, like every
other "that group is not available" case, instead of `500`.

## Plan

`export::rendition::is_cache_miss` classifies a group fetch failure as a 404
only for the `NotFound`, `Old`, and `Evicted` wire codes. A publisher that
disconnects resets the fetch with code 0, which arrives as
`moq_net::Error::Remote(0)`, so `Rendition::segment` returns `Err` and the
serve path answers `500`.

Observed end to end against a local relay: with a rendition bound to a
publisher that then disconnected, every segment still listed in the playlist
window answered `500` (`hls request failed err=moq: remote error: code=0`)
until it aged out of the window. The playlist keeps listing those segments
because the timeline it renders from is a different track, and often a
different broadcast, from the media.

Both statuses are wrong in different directions, so decide deliberately: `500`
tells a CDN or player to retry a segment that can never be served, while `404`
says it is gone. Reaching for a broader "is this retryable" classification is
what the root guide warns against, so name the codes that mean the media is
gone rather than inverting the test.

Reachable on both rendition shapes (the catalog's own broadcast and a named
sibling), since both hold the broadcast the catalog was read from.
