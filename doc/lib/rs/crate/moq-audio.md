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
| `decode` | A subscribed track back to PCM: Opus, PCM, or AAC-LC | always (AAC via `aac`) |
| `playback` | PCM out a speaker, mixing every track into one device stream | `playback` |
| `aec` | Subtract what the speaker plays from what the microphone hears | `aec` |

The codecs and DSP are Rust, all the way down. Opus is `unsafe-libopus` (libopus
transpiled, not wrapped), AAC decode is
[`symphonia`](https://crates.io/crates/symphonia-codec-aac), resampling is
`rubato`, echo cancellation is [`sonora`](https://crates.io/crates/sonora), a
port of WebRTC's audio processing, and devices go through `cpal`. So there is no
C toolchain, no CMake step, and no codec to install on the host.

The one system dependency is the platform's audio API, and only when you enable
`capture` or `playback`: CoreAudio and WASAPI ship with the OS, but a Linux build
needs the ALSA development headers (`libasound2-dev` or your distro's equivalent)
for cpal to link. A default build has neither feature and needs nothing.

`Frame` is a timestamp, a payload, and an `Activity`. Layout lives on the
producer or consumer (`encode::Input` / `decode::Config`) rather than on each
frame, so you cannot drift between calls, and `Format` mirrors the WebCodecs
`AudioData.format` values with conversions to the interleaved `f32` that the
codecs want. `Activity` says whether a packet coded audio or none at all, which is what an
Opus sender withholding silence looks like (`encode::Config::dtx`), so a call UI
can show who is talking without running a second voice detector; the publish side
reads the same thing off `encode::Producer::activity`. It is read off the packet,
so it works for senders that are not us. Opus marks withheld audio but not
silence itself, so a silent run is interrupted every few hundred milliseconds by
an ordinarily coded frame that reads active: hold an indicator across that gap
rather than following it frame by frame.

## Installation

Everything is on by default: AAC decode, device capture and playback, and echo
cancellation. On Linux the device features link ALSA at build time (`libasound2-dev`
on Debian and Ubuntu), so a service that only encodes or relays can drop them:

```bash
cargo add moq-audio --no-default-features --features aac
```

| Feature | Default | Pulls in |
| --- | --- | --- |
| `aac` | yes | AAC-LC decode via `symphonia`, pure Rust |
| `capture` | yes | Microphones via `cpal`; macOS system audio via ScreenCaptureKit |
| `playback` | yes | Speaker output via `cpal` |
| `aec` | yes | Acoustic echo cancellation (`sonora`); implies `capture` and `playback`, since the canceller taps the output mix as its reference |
| `pipewire` | no | cpal's native PipeWire host on Linux, linking `libpipewire-0.3` |
| `pulseaudio` | no | cpal's native PulseAudio host on Linux, linking `libpulse` |

Without a native host, cpal reaches a Linux sound server through ALSA's `default`
device, which desktops route to PipeWire or PulseAudio via their ALSA plugin.

## Publishing

`encode::Publication` advertises the track up front and opens the microphone
only while somebody is listening. Its separate driver owns capture and encoding,
while clones of the retained handle control that same track:

```rust
use moq_audio::{capture, encode};
use tokio::task::LocalSet;

let local = LocalSet::new();
local.run_until(async move {
    let mut options = encode::PublicationOptions::default();
    options.capture = capture::Config::default();
    options.clock = clock;

    let (mut microphone, driver) = encode::Publication::new(broadcast, catalog, options)?;
    let publish = tokio::task::spawn_local(driver.run());

    microphone.stop(); // releases the device, but keeps the track
    microphone.replace(capture::Source::Microphone(Some(device_id)));
    microphone.start(); // resumes on the same MoQ broadcast and track

    if let Some(state) = microphone.changed().await {
        tracing::info!(
            status = ?state.status(),
            device = ?state.device(),
            failure = ?state.failure(),
        );
    }
    let level = microphone.level(); // rms/peak, for a local meter

    // The driver runs until the track ends or the last handle drops, so
    // releasing the controls is how this flow finishes.
    drop(microphone);
    publish.await??;
    Ok(())
}).await?;
```

The native capture driver is not `Send`, so await it directly or use a
`LocalSet` when it needs a separate task. `encode::publish_capture` remains the
shorthand when no controls are needed.

Constructing a publication never touches the device, so the controls exist even
on a machine with no microphone. The driver probes the input for the PCM layout
the catalog rendition describes and registers that rendition on the first
success, so the catalog advertises no audio until then and `Status::Starting` is
how an observer sees it. Transient input failures retry with capped backoff.
Terminal failures leave the track registered in `Status::Failed`; `start`
retries, while `replace` selects another device without changing the identity
subscribers already know. `Publication::level` is measured
after AEC and other capture processing, so it is suitable for a local meter or
active-speaker input. It rides its own channel rather than `State`, because it
changes every buffer and `changed` reports lifecycle transitions only.

`encode::Producer` takes PCM you supply instead. Either way the codec is
`encode::Codec`: Opus (the default) or uncompressed PCM, which trades bandwidth
for the lowest possible latency and no codec delay.

## Subscribing and playback

`decode::Consumer` reads the catalog entry to pick a decoder and resamples to
whatever rate you ask for. Alongside the two codecs this crate encodes, it reads
AAC-LC, which is what a broadcast that arrived through a gateway (RTMP, SRT, HLS,
gstreamer) carries. That is decode only: there is no Rust AAC encoder, so publish
Opus. HE-AAC is rejected rather than half-decoded, whether its config says so up
front (`mp4a.40.5` / `.29`) or hides the SBR in a sync extension after an AAC-LC
header; only a stream that signals SBR in band alone slips through, and plays as
its LC core. The `aac` feature (on by default) drops the decoder for a
publish-only build. `playback::Engine` owns the output device, and each
`playback::Sink` is one stream mixed into it:

```rust
use moq_audio::{decode, playback};

// `rendition` is the hang catalog's AudioConfig for the track you want.
let mut audio = decode::Consumer::new(&broadcast, &rendition, "audio", decode::Config::default()).await?;

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
