# [M] HLS timeline resubscribe

## Goal

A transient error on a timeline subscription does not freeze the playlists
or turn a rendition's remaining segments into `EXT-X-GAP`. The watcher subscribes again
and keeps resolving segments. Only a clean end, a malformed timeline, or the
track or broadcast going away ends its spans.

## Plan

In [#4280](https://github.com/moq-dev/moq/pull/4280), `watch_spans` in
`rs/moq-hls/src/export/rendition.rs` logs any error and then calls
`spans.end()`, so rows past the last record resolve as gaps for good. Codex
flagged it as a P1
([r4112645622](https://github.com/moq-dev/moq/pull/4280#discussion_r4112645622)).
The agent declined it because the watcher never retries, and named
re-subscribing as the real fix. The maintainer's decision in the 09-28
merged-PR audit is to do that re-subscribe.

The reference rendition has the same flaw on a separate path, and a worse
one: `watch_timeline` in `rs/moq-hls/src/export/mod.rs` warns on the error,
then closes every window, so all playlists freeze and every recording cursor
ends. It follows the same rules below.

- Re-subscribe only on a recoverable error (a transport reset, a lost
  session, the publisher's track aborting) while the broadcast is still live,
  resuming from the records the spans already hold rather than rebuilding
  them. A malformed or unsupported timeline (a bad archive timescale, broken
  JSON or DEFLATE) would replay the same retained record forever, so it fails
  loud and ends the spans instead.
- Re-subscribe on the event that makes it possible (the broadcast or track
  being available again), not on a timer.
- The spans end on a clean end of the timeline, when the broadcast goes
  away, or when a re-subscribe is refused because the track is no longer
  published. That last case is the terminal state for a timeline that never
  returns while the broadcast stays live on other tracks: its remaining rows
  resolve as gaps. The concern the decline raised still holds, so nothing may
  park forever (`poll_resolved`, a recording `segments::Consumer`); a
  re-subscribe is either served, refused, or cut short by the broadcast
  ending.

Add regression tests where a non-reference rendition's timeline errors and
comes back, and its later segments resolve to media, not gaps; and where the
reference timeline does the same, and every playlist keeps advancing. One
reconnects past `timeline::CHECKPOINT_RECORDS` on a durable timeline: the
fresh decoder's leading `Skip` covers both records the source popped during
the outage and retained records the checkpoint omitted. Drop only the rows
before the new logical offset and keep the retained ones, so older segments
stay listed and popped ones go; test it with pops during the outage.

## Related

- [Per-track timelines](/quest/m1/archive/track-timeline/README.md) - the line this blocks
