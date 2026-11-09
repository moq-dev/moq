# [M] ID3 in the catalog's events section

## Goal

A Hang catalog exposes ID3 metadata as an ID3 scheme entry in the shared
`events` section [emsg](/quest/m5/emsg.md) defines, with named tracks and
media correlation, independent of the MPEG-TS PID that supplied it.
Consumers can discover and read timed ID3 without understanding the `mpegts`
extension, while MPEG-TS export still reconstructs a valid ID3 elementary
stream.

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer for timed metadata.

Define the ID3 scheme's entry and frame contract for complete ID3 tags on the
shared event contract [emsg](/quest/m5/emsg.md) settles: one catalog `events`
section keyed by scheme URI, not a top-level `id3` section (decided 2026-11-07, maintainer, /quest-plan from the API sweep of open PRs and remaining quests). Publish tags when
received on independently sequenced groups, with their presentation time on
the broadcast clock and an optional association to the program's rendition.
ID3-only programs need no artificial media owner. Carry original tag bytes,
and in the scheme's entry the raw stream-level PMT descriptor loop plus any
application or program registration metadata the exporter needs to reproduce
the input. Do not parse the tag into a fixed
metadata vocabulary; applications can decode frames with their ID3 library and
new tag types remain forward-compatible.

Teach MPEG-TS import to use this entry only when the PMT descriptors or
application metadata identify ID3 and complete payloads validate as ID3 tags.
Stream type `0x15` alone is generic metadata PES and does not qualify.
Ambiguous, non-ID3, or inconsistent streams stay byte-faithful in the generic
`mpegts` representation. Export typed ID3 to recreate the elementary stream,
descriptors, and timestamps, without publishing a second generic copy of a
stream positively identified as ID3.

Land matching Rust and web catalog bindings plus byte-faithful round-trip tests
with default and non-default PMT descriptors, multiple tags, unknown frames,
large tags spanning PES packets, timestamp wrap, discontinuity, and an ID3-only
program. Include non-ID3 and malformed stream type `0x15` fixtures that remain
generic.

## Required

- [fMP4 emsg](/quest/m5/emsg.md) - settles the shared timed-metadata contract this builds on, which needs maintainer agreement first
