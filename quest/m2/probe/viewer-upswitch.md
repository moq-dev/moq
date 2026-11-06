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
[probe epic](/quest/m2/probe/README.md). Players
set the viewer target (`viewer` on `Watch.Player`), the session sends it as its PROBE target
([moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090)), and the relay's sender probes the downlink up to it; this
quest makes selection act on the estimate that rises. The relay runs
moq-quic, whose native probing ([moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)) is that
sender.

Selection reads its broadcast's viewer grant (`Bandwidth.Viewer.rate`), not
the connection's whole
`estimatedRecvRate`, so several watchers on one pooled connection don't each
select against the full downlink (decided with the maintainer on 2026-10-19).

The player's viewer ceiling already covers the selection margin and audio:
it is the best enabled video rendition's bitrate over 0.8 (the rate at which
`Video.Source` selects it, its `out.ceiling`) plus the selected audio
rendition's bitrate. A grant is capped at the ceiling, so selection keeps its
0.8x discount but subtracts the audio bitrate from the grant first; a full
grant then selects the best rendition.

## Closes

- [#4773](https://github.com/moq-dev/moq/issues/4773) - close this issue when the quest finishes
