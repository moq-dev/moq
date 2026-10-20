# [M] A viewer on a small rendition can find headroom and move up

## Goal

A `@moq/watch` viewer with no `target.bitrate`, whose receive estimate is
capped by the small rendition it plays, can confirm spare capacity and select
a larger rendition, without pinning by name.

## Plan

`js/watch/src/video/source.ts` caps automatic selection at 0.8 times
`estimatedRecvRate`. The estimate only follows what is being sent, so it stays
app-limited, and `js/net` never sends a PROBE target to request headroom above
it.

Decided (2026-10-04): its own quest, after the transport can validate
headroom. The viewer sends a PROBE target and selection uses the validated
estimate.

Decided with the maintainer on 2026-10-17: this joined the
[probe epic](/quest/m2/probe/README.md). [Players](/quest/m2/probe/players.md)
set the viewer target, the [session](/quest/m2/probe/target.md) sends it as
its PROBE target, and the relay's sender probes the downlink up to it; this
quest makes selection act on the estimate that rises. The relay runs
moq-quic, so it requires [native probing](/quest/m2/probe/native.md); if that
ends in a measured no-go, require [padding](/quest/m2/probe/padding.md)
instead rather than leaving this blocked.

Selection reads its broadcast's viewer grant from the
[target](/quest/m2/probe/target.md) quest, not the connection's whole
`estimatedRecvRate`, so several watchers on one pooled connection don't each
select against the full downlink (decided with the maintainer on 2026-10-19).

Selection discounts the estimate by 0.8x, so a probe that only reaches the
rendition's bitrate never makes it selectable. Settle with
[players](/quest/m2/probe/players.md) whether its handle covers that margin
and audio, or selection stops discounting a probed estimate.

## Required

- [Session probe target](/quest/m2/probe/target.md) - the viewer handle the session sends as its PROBE target
- [Native probing](/quest/m2/probe/native.md) - the relay validates headroom above the media it sends
- [Players set viewer targets](/quest/m2/probe/players.md) - `@moq/watch` asks for its highest enabled rendition

## Closes

- [#4773](https://github.com/moq-dev/moq/issues/4773) - close this issue when the quest finishes
