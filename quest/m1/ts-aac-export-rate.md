# [S] A TS AAC source exports to TS with default settings

## Goal

`moq export ts` of an MPEG-TS AAC source, packed as ffmpeg packs it by
default, succeeds with default settings: the multiplex rate it picks covers
what the audio needs, so it never fails with "MPEG-TS output missed a decode
deadline". Non-goal: pacing the multiplex on its PCR, which
[its own quest](/quest/m3/ts-export-pace.md) owns.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan): ✅ m1 (rejected: m3 next to the
other TS export quests; drop it), since a default export of a common source
fails outright.

Found 2026-11-24 while measuring
[moq-dev/moq#9561](https://github.com/moq-dev/moq/pull/9561), on its base
commit: `moq import ts` of an ffmpeg TS AAC source, then `moq export ts`,
failed with "MPEG-TS output missed a decode deadline on PID 256 at 158726 b/s
(estimated needed rate 399366 b/s)". The error comes from
`rs/moq-mux/src/container/ts/schedule.rs`. Reproduce it first, then find
where the export sizes the multiplex: ffmpeg packs about 186 ms of AAC per PES
packet, so a rate estimated from per-frame sizes or an early window may
undercount a packet that arrives whole. Find the PR that introduced the
estimate by blame for `## Caused by`.

## Related

- [TS export pacing](/quest/m3/ts-export-pace.md) - the same export's release timing
