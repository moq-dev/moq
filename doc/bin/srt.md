---
title: SRT
description: SRT contribution and playback
---

# SRT

`moq import srt` accepts SRT pushes (`--listen`) or pulls from a remote SRT
source (`--connect`); `moq export srt` serves SRT to players or pushes to a
remote. The payload is MPEG-TS, so the same codecs and behavior as
[`import ts`](/bin/cli) apply: H.264/H.265 video and AAC, MP2, AC-3, or E-AC-3
audio. A damaged unit is dropped and the session stays up; video resumes at its
next keyframe, freezing for up to one GOP.

```bash
# Accept a contribution feed and publish it
moq --connect https://relay.example.com/anon --broadcast event.hang import srt --listen '[::]:9000'

# Serve a broadcast to SRT players
moq --connect https://relay.example.com/anon --broadcast event.hang export srt --listen '[::]:9000'
ffplay srt://localhost:9000

# Pull from a remote encoder
moq --connect https://relay.example.com/anon --broadcast event.hang import srt --connect 'srt://encoder.example.com:9000?streamid=live/cam'
```

A multi-program feed is refused unless `--program`
picks one: `--program 2` imports program 2 alone, and `--program all`
publishes each program as its own broadcast (`event.hang` becomes
`event/1.hang`, `event/2.hang`, and so on).

```bash
moq --connect https://relay.example.com/anon --broadcast event.hang import srt --listen '[::]:9000' --program all
```

`--latency` (default 500 ms) sets the SRT receive buffer and doubles as the
skip threshold on export. A `--connect` URL needs a `streamid` query or a path;
a listener bridges one `--broadcast` and ignores the stream id it is offered.
The library is [`moq-srt`](https://docs.rs/moq-srt).
