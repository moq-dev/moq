---
title: SRT
description: SRT contribution and playback
---

# SRT

`moq import srt` accepts SRT pushes (`--listen`) or pulls from a remote SRT
source (`--connect`); `moq export srt` serves SRT to players or pushes to a
remote. The payload is MPEG-TS, so the same codecs as [`import ts`](/bin/cli)
apply: H.264/H.265 video and AAC, MP2, AC-3, or E-AC-3 audio.

```bash
# Accept a contribution feed and publish it
moq --client-connect https://relay.example.com/anon --broadcast event.hang import srt --listen '[::]:9000'

# Serve a broadcast to SRT players
moq --client-connect https://relay.example.com/anon --broadcast event.hang export srt --listen '[::]:9000'
ffplay srt://localhost:9000

# Pull from a remote encoder
moq --client-connect https://relay.example.com/anon --broadcast event.hang import srt --connect 'srt://encoder.example.com:9000?streamid=live/cam'
```

`--latency` sets the SRT receive buffer and doubles as the skip threshold on
export. A `--connect` URL needs a `streamid` query or a path; a listener
bridges one `--broadcast` and ignores the stream id it is offered. The
library is [`moq-srt`](https://docs.rs/moq-srt).
