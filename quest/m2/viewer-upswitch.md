# [M] A viewer on a small rendition can find headroom and move up

## Goal

A `@moq/watch` viewer with no `target.bitrate`, whose receive estimate is
capped by the small rendition it plays, can confirm spare capacity and select
a larger rendition, without pinning by name.

## Plan

`js/watch/src/video/source.ts` caps automatic selection at 0.8 times
`estimatedRecvRate`. The estimate only follows what is being sent, so it stays
app-limited, and `js/net` never sends a PROBE target, so the relay never pads
above it.

Decided (2026-10-04): its own quest, after the transport can validate
headroom. The viewer sends a PROBE target, the relay pads up to it, and
selection uses the validated estimate. If [quic-probe](/quest/m2/quic-probe.md)
ends in a measured no-go, revisit this quest rather than leaving it blocked.

## Required

- [Discover capacity](/quest/m2/quic-probe.md) - a validated estimate above the media bitrate

## Closes

- [#4773](https://github.com/moq-dev/moq/issues/4773) - close this issue when the quest finishes
