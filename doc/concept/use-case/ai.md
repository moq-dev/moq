---
title: MoQ for AI
description: Voice agents, streamed generation, and on-demand inference over MoQ
---

# MoQ for AI

Real-time AI has needs WebRTC wasn't designed for, and MoQ lines up with them.

## Reliability you can tune

Inference is slow and expensive. If you spend 300 ms and real money on a
response, you want a say in how the answer survives packet loss. WebRTC favors
low playout latency and leaves recovery to the implementation; MoQ lets the
application pick: audio near-lossless with a
latency budget, video skippable, prompts fully reliable, all on one connection.

## Faster than real time

A TTS model emits a whole sentence in a burst with timestamps in the future.
WebRTC keeps buffering and playout inside its media engine; MoQ exposes the
timestamped burst to the application, and the player paces it. In the browser,
set a latency ceiling on [`<moq-watch>`](/lib/js/watch#buffered-playback) to
buffer ahead and call `reset()` on an interruption. The
[Pipecat](https://github.com/pipecat-ai/pipecat) voice-agent framework ships
a MoQ transport built on this.

## Inference on demand

Announcements and catalog metadata can advertise tracks before their frames
exist. Media frames are transmitted only once someone subscribes to the track,
and a publisher can defer encoding until then too. A `captions` track backed by
Whisper runs only while a viewer has captions on. Object detection can consume
a 360p 10 fps rendition while the full-resolution track stays idle until a
human asks for it.

## Media and data together

Send prompts, tool calls, transcripts, or vertex data as their own tracks next
to the audio. The relay treats them all the same, and a JSON snapshot track
gives late joiners the current state in one step.

## In practice

Server-side agents use [Rust](/lib/rs/) with hardware codecs, or
[Python](/lib/py/) where the ML stack lives. Browsers use
[`@moq/publish`](/lib/js/publish) for the microphone and
[`@moq/watch`](/lib/js/watch) for playback. Background reading:
[WebRTC is the problem](https://moq.dev/blog/webrtc-is-the-problem).
