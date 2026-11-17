# [M] The fMP4 export holds back reading, not memory, until its init segment

## Goal

Before it writes its init segment, the fMP4 export
(`rs/moq-mux/src/container/fmp4/export.rs`) holds a bounded share of each
track instead of queueing every other track's fragments for up to 30 s of
their own timestamps. A broadcast whose video never becomes describable, or
whose timestamps never advance, costs a subscriber bounded memory and ends
with `TrackUndescribed`.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source.
`INIT_QUEUE` compares a track's frame timestamps with its first one, so a
publisher with frozen or bouncing timestamps never trips it, and every
fragment goes into `Init::Waiting` while an undescribable rendition holds the
init. That memory sits in `moq-cli subscribe`, moq-video's decoder, and
moq-transcode on a server. `release` held at most one pending frame per
track, as the MKV, FLV, and TS exports on `main` still do.

Recommended: stop polling a track once it has queued its share before the
init, the way the other exports gate on the jitter buffer's `full`, so the
backlog stays in the transport cache that the subscription's max delay
already bounds; and time the init wait by arrival on the local clock, not by
the publisher's timestamps.

Test: a simulated-clock export with one undescribable video track and an
audio track whose timestamps never advance; assert the queue stays bounded
and the export fails with `TrackUndescribed` after the wait.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#4883](https://github.com/moq-dev/moq/pull/4883) - queued fragments behind the init with only a media-time limit
