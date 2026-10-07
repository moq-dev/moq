# [M] JS data consumers return each value's timestamp

## Goal

`@moq/json` and `@moq/flate` snapshot and stream consumers return each value
with its frame's timestamp, so a browser can hold telemetry back until the
video playhead reaches it. Mirrors
[Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) in Rust.

## Plan

Requested by OneTooMany, whose web frontend shows KLV and MAVLink telemetry
beside video and today reads raw frames to recover the timestamps.

Decided (2026-10-01): `next()` and the async iterator yield `@moq/net`'s
`Timed<T>` (`{ value, at? }`), the type the producers take after
[JS publishing never invents a timestamp](/quest/m1/js-publish-timestamp.md).
`at` is the frame's timestamp, absent for an untimed frame. Decided
(2026-10-05, types settled 2026-10-06): timedness is per track, as the
[untimed model](/quest/m1/untimed-model.md) decided and `@moq/net` mirrors:
`timescale` is optional, an untimed track's frames have no `at`, and a frame
whose timedness doesn't match its track is refused. Snapshot
consumers get the same two reads as Rust: `next()` yields every state in
order (`@moq/json` stops draining to the latest, `@moq/flate` reads groups in
order), and `latest()` skips to the newest state, today's behavior. A reader
that falls behind still jumps to the newest group. Update
`doc/lib/js/{json,flate}.md`.

Public API: breaking. Wire: none.

## Required

- [JS publishing never invents a timestamp](/quest/m1/js-publish-timestamp.md) - adds `Timed<T>` to `@moq/net`

## Related

- [Synced data playback](/quest/m2/watch-data-sync.md) - the js/watch reader built on these timestamps
