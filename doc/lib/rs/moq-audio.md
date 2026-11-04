---
title: moq-audio
description: Native audio capture, codecs, playback, and echo cancellation
---

# moq-audio

[![crates.io](https://img.shields.io/crates/v/moq-audio)](https://crates.io/crates/moq-audio)
[![docs.rs](https://docs.rs/moq-audio/badge.svg)](https://docs.rs/moq-audio)

The audio half of a native call: microphone in, hang track out, speaker at
the far end. `moq import capture` and `moq play` use it. There is no C
toolchain or codec package to install, apart from ALSA headers on Linux when
capture or playback is enabled.

| Module | Does |
| --- | --- |
| `capture` | Microphones via CoreAudio, WASAPI, ALSA (and PipeWire/PulseAudio hosts), plus macOS system audio |
| `encode` | PCM to Opus (mono or stereo, with DTX and voice activity) or raw PCM |
| `decode` | Opus (up to 7.1), PCM, and AAC-LC (mono or stereo) back to PCM, resampled to the rate you want |
| `playback` | One output device mixing every track in a call, with click-free volume ramps |
| `aec` | Acoustic echo cancellation (a port of WebRTC's) for caller-supplied PCM or the built-in devices |
| `bus` | Routes sources into several mixes, each with its own meter and mix-minus, advanced on the caller's clock |

Open one `capture::Producer`, give a consumer to each bus channel or encoder,
and run the producer alongside them. They share samples and capture timestamps;
a slow reader loses only its own expired audio. Dropping the producer releases
the device, and a device failure ends every reader with the same error.

The bus owns no clock: whoever owns the audio clock, such as a program
clock on `kio::clock` or a device callback, advances its one driver by the
frames that clock moved, so tests step it on virtual time. Each mix sums only
the sources routed into it, and its readers get blocks stamped with their
position on that clock, so a reader that falls behind skips its own audio
without stalling the mix.

Without a subscriber the encoder keeps raw PCM
up to the track's `max_age`, then encodes that history before continuing live when
someone subscribes. With an unlimited `max_age` it keeps at most 30 seconds
(about 11.5 MB for 48 kHz stereo f32). Set `encode::Capture::on_demand` to false
to encode continuously instead. Stopping an encoder leaves the shared capture running, and the
history it retained stays on the track until it is subscribed or expires; a device failure keeps
it the same way before the encoder returns the error. Its consumer can be swapped without
changing the track subscribers know. Opus packs 20 ms per packet by default,
or 10 ms with a shorter `frame_duration`. Groups carry at least 20 ms by default,
so 10 ms packets pair up. `encode::Options::group_duration`
changes that minimum; zero puts every packet in its own group. Longer groups
mean fewer streams per relay but coarser loss. There is no AAC encoder, so an
AAC encode request is refused; AAC decode is for broadcasts from the ingest gateways.

Playback writes never block: samples that do not fit are dropped and
reported, and retrying them would only add latency. Voice activity is read off the Opus
stream, so a call UI needs no second detector. Echo cancellation pairs one
playback engine with one live microphone, or processes playback and microphone
PCM supplied by another audio stack. A microphone buffer is stamped at
the capture instant of its first sample, so it lines up with video acquired
then: up to 10 ms later with echo cancellation on, and at the producer's read time on a host with no usable capture timestamp.

```bash
cargo add moq-audio --features playback
cargo add moq-audio --features aec    # Built-in devices; Linux: cpal links libasound
cargo add moq-audio                   # PCM echo processing needs no device feature
cargo add moq-audio --features capture,pipewire
```

`pipewire` and `pulseaudio` only take effect alongside `capture` or `playback`.

API: [docs.rs/moq-audio](https://docs.rs/moq-audio). Pair with
[`moq-video`](/lib/rs/moq-video). The playout target `moq play` and
`<moq-watch>` share is [audio jitter](/concept/audio-jitter).
