---
title: SRT
description: SRT contribution and playback
---

# SRT

`moq import srt` accepts SRT pushes (`--listen`) or pulls from a remote SRT
source (`--connect`); `moq export srt` serves SRT to players or pushes to a
remote. The payload is MPEG-TS, so the same codecs as [`import ts`](/bin/cli)
apply: H.264/H.265 video and AAC, MP2, AC-3, or E-AC-3 audio. Ingest logs the
same per-stream lines as `import ts`, under an `srt{path=...}` span: an
elementary stream that stopped delivering access units, audio frame sync lost,
damaged units refused on each PID, and the TR 101 290 counters when one moves.
They grade the TS as SRT delivered it, after retransmission. Damage drops that
unit and keeps the session alive; video closes its group at the break and
resumes at its next keyframe, freezing for up to one GOP.

```bash
# Accept a contribution feed and publish it
moq --connect https://relay.example.com/anon --broadcast event.hang import srt --listen '[::]:9000'

# Serve a broadcast to SRT players
moq --connect https://relay.example.com/anon --broadcast event.hang export srt --listen '[::]:9000'
ffplay srt://localhost:9000

# Pull from a remote encoder
moq --connect https://relay.example.com/anon --broadcast event.hang import srt --connect 'srt://encoder.example.com:9000?streamid=live/cam'
```

Import publishes the feed's own PTS and anchors the catalog clock on its first
frame, as [`import ts`](/bin/cli) does. Each connection publishes under a fresh
[epoch](/concept/moq-lite#publisher-epochs) per broadcast: an encoder that
reconnects while its stale connection is still open replaces it at once.
Subscriptions to the stale feed end with `Unroutable` instead of stalling, and
a viewer's next subscribe reaches the new feed.

A multi-program feed is refused, as with `import ts`, unless `--program`
picks one: `--program 2` imports program 2 alone, and `--program all`
publishes each program as its own broadcast (`event.hang` becomes
`event/1.hang`, `event/2.hang`, and so on).

```bash
moq --connect https://relay.example.com/anon --broadcast event.hang import srt --listen '[::]:9000' --program all
```

`--latency` sets the SRT receive buffer and doubles as the export's jitter
buffer delay, as `export ts --delay` in the [CLI](/bin/cli): each frame is muxed that
long after its decode time, and one arriving later is dropped. Export paces each SRT payload on the media clock, and re-anchors that
pacing on a declared marker, so a restarted timeline plays out from the
live edge instead of stalling until it catches up. A `--connect` URL needs a `streamid` query or a path; a listener
bridges one `--broadcast` and ignores the stream id it is offered. The
library is [`moq-srt`](https://docs.rs/moq-srt).
