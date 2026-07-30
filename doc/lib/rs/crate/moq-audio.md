---
title: moq-audio
description: Native audio capture, encoding, decoding, playback, and echo cancellation for MoQ
---

# moq-audio

[![crates.io](https://img.shields.io/crates/v/moq-audio)](https://crates.io/crates/moq-audio)
[![docs.rs](https://docs.rs/moq-audio/badge.svg)](https://docs.rs/moq-audio)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

The native audio stack, and the counterpart to
[moq-video](/lib/rs/crate/moq-video): a microphone in, a
[hang](/lib/rs/crate/hang) track out, and a speaker at the far end.

## Overview

| Module | Does | Feature |
| --- | --- | --- |
| `capture` | Microphone (CoreAudio / WASAPI / ALSA) or macOS system audio | `capture` |
| `encode` | PCM to Opus or PCM, published through `moq-mux` | always |
| `decode` | A subscribed track back to PCM | always |
| `playback` | PCM out a speaker, mixing every track into one device stream | `playback` |
| `aec` | Subtract what the speaker plays from what the microphone hears | `aec` |

Everything is pure Rust. Opus is `unsafe-libopus`, devices are `cpal`, resampling
is `rubato`, and echo cancellation is [`sonora`](https://crates.io/crates/sonora),
a port of WebRTC's audio processing. There is no C toolchain, CMake step, or
system codec anywhere in the graph.

`Frame` is a timestamp and a payload. Layout lives on the producer or consumer
(`encode::Input` / `decode::Config`) rather than on each frame, so you cannot
drift between calls, and `Format` mirrors the WebCodecs `AudioData.format` values
with conversions to the interleaved `f32` that the codecs want.

## Installation

Device I/O is off by default, so a service that only encodes or relays pulls in
neither `cpal` nor (on Linux) the ALSA build dependency:

```bash
cargo add moq-audio --features capture,playback,aec
```

`aec` implies both `capture` and `playback`, since the canceller taps the output
mix as its reference.

## Publishing

`encode::publish_capture` advertises the track and catalog up front and opens the
microphone only while somebody is listening:

```rust
use moq_audio::{capture, encode};

let config = capture::Config::default();

encode::publish_capture(
    broadcast,
    catalog,
    config,
    encode::Options::default(),
    clock,
).await?;
```

`encode::Producer` takes PCM you supply instead. Either way the codec is
`encode::Codec`: Opus (the default) or uncompressed PCM, which trades bandwidth
for the lowest possible latency and no codec delay.

## Subscribing and playback

`decode::Consumer` reads the catalog entry to pick a decoder and resamples to
whatever rate you ask for. `playback::Engine` owns the output device, and each
`playback::Sink` is one stream mixed into it:

```rust
use moq_audio::playback;

let engine = playback::Engine::open(playback::Config::default()).await?;
let mut sink = engine.sink(playback::Input {
    sample_rate: audio.sample_rate(),
    channels: audio.channels(),
    ..Default::default()
})?;

while let Some(frame) = audio.read().await? {
    sink.write(&frame.data)?;
}
```

That split is deliberate: a call with several participants mixes into one device
stream instead of opening one per speaker, which is what the device wants and what
echo cancellation needs a single reference from.

The device is not your problem. Opening it, renegotiating its format, resampling
to its rate, and reopening it after it disappears all happen on a driver thread
behind the `Engine`. A `Sink` keeps taking writes throughout and its samples land
wherever the device currently is. Volume changes ramp over a few milliseconds, so
there is no click on mute; `set_volume(0.0)` is the pause.

`Sink::buffered()` reports how much audio sits between your last write and the
hardware callback. That is the pacing signal an A/V sync clock steers by: audio
plays at exactly the device's rate, so video follows it rather than the other way
around.

## Echo cancellation

Without it, anyone on a laptop with no headset sends the call back to itself. A
`Canceller` comes from the `Engine` doing the playing, because cancelling an echo
means knowing what was played, and goes into the capture config so the microphone
you publish is already clean:

```rust
use moq_audio::{aec, capture, playback};

let engine = playback::Engine::open(playback::Config::default()).await?;

let mut capture = capture::Config::default();
capture.aec = Some(engine.canceller(aec::Config::default()));
```

One canceller belongs to one microphone: it holds the adaptive filter modelling
the path from that speaker to that microphone. Clones share it, so clone for a
mute button on a UI thread, not to run a second capture.

The work runs in the microphone callback on 10 ms frames, which is where the up-to
10 ms of added capture latency comes from. Both the reference and the microphone
are processed there, in that order, because that is the ordering the echo model
needs. It applies to microphones only: macOS system audio is already the output,
so there is nothing to subtract from it.

## Devices

```rust
moq_audio::capture::devices().await?;   // inputs
moq_audio::playback::devices().await?;  // outputs
```

Both hand back ids that go straight into `capture::Config::source` and
`playback::Config::device`; `None` opens the system default.

## API Reference

Full API documentation: [docs.rs/moq-audio](https://docs.rs/moq-audio)

## Next Steps

- Pair it with [moq-video](/lib/rs/crate/moq-video) for the other half of a call
- Publish through [hang](/lib/rs/crate/hang) catalogs and [moq-mux](/lib/rs/crate/moq-mux) containers
- Capture and publish from the command line with [moq-cli](/bin/cli)
