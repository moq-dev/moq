# [XL] moq-cli: remaining work for capture and playback (window/app capture, device enumeration, native player)

## Goal

Implement and verify the behavior tracked in [#2272](https://github.com/moq-dev/moq/issues/2272)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Tracking issue for the gaps in `moq import capture` and the (nonexistent) native playback path, in the same spirit as #1837 (which tracks the `moq-video` backend work this sits on top of). Audit done against `rs/moq-cli`, `rs/moq-video`, and `rs/moq-audio` on the `dev` branch.

Goal: `moq` should be able to capture **a window / an app / a screen / a microphone or device**, and play a broadcast back **like `ffplay`**, without an external ffmpeg.

Current state:

- **Capture** (`moq import capture`, feature-gated): camera (AVFoundation / Media Foundation / V4L2) and whole-display (ScreenCaptureKit / DXGI Desktop Duplication / xdg-desktop-portal + PipeWire), encoded to H.264/H.265; microphone via cpal, encoded to Opus. Flags today are `--camera`, `--screen`, `--width/--height/--fps/--bitrate/--codec`, `--hardware/--software`, `--microphone`, `--audio-bitrate`, `--no-video/--no-audio`.
- **Playback**: none. `moq export <container>` writes fmp4/mkv/ts/flv/h264/h265 to stdout; playing it means piping to ffplay (`doc/bin/cli.md:205`). `moq-video::decode` exists on `dev` but `moq-cli`'s export path never calls it.

---

##### Shipping: no released binary can capture at all

- \[ ] **`capture` is off by default and no shipped artifact turns it on.** `rs/moq-cli/Cargo.toml` omits `capture` from `default`, and every distribution builds default features: Nix (`nix/overlay.nix:59`, `cargoExtraArgs = "-p moq-cli"`), Docker, winget, and `cargo install moq-cli`. So `moq import capture` is unreachable for every user who doesn't build from source with `--features capture`. The feature is off for good reasons (Linux `capture` pulls libclang + V4L2 headers via bindgen, and ALSA via cpal), so pick one: default-on with the existing trimmable sub-features, or a separate `moq-cli-capture` artifact. (`doc/bin/cli.md` does document the `--features capture` build, so this is a packaging gap, not a documentation one.)

##### Capture sources

- \[ ] **Window capture.** `capture::Source` is `Camera | Display` only (`rs/moq-video/src/capture/mod.rs:62`). Per platform: macOS already links `SCWindow` but only ever builds `SCContentFilter::initWithDisplay_excludingWindows` (`screencapture.rs:55-57`); Windows needs `Windows.Graphics.Capture` since Desktop Duplication is whole-monitor by construction (`desktopduplication.rs:5-7`); Linux asks the portal for `SourceType::Monitor` only (`pipewire.rs:162`), so the picker does not even offer windows. `Source` is `#[non_exhaustive]`, so adding a variant is additive.
- \[ ] **Application capture** (every window of a process, follows new windows). macOS gets this nearly free from `SCShareableContent` applications + a content filter; Windows/Linux need per-window composition or a portal that supports it.
- \[ ] **Region / crop capture.** No knob anywhere; `capture::Config` is `source`/`device`/`width`/`height`/`framerate`.
- \[ ] **CLI cannot pick a display.** The macOS and Windows backends *do* support display selection by index (`screencapture.rs:41-52` accepts `N` or `display:N`; `desktopduplication.rs:281-292` via `EnumOutputs`), but `CaptureArgs::video_config` (`rs/moq-cli/src/publish.rs:326-332`) only sets `config.device` in the camera branch, and `--camera` `conflicts_with = "screen"`. So `--screen` always captures the main display and the library capability is unreachable. Needs a `--display <index>` (Linux would ignore it, see below).
- \[ ] **Linux display selection.** The portal owns source selection and the device selector is explicitly ignored (`pipewire.rs:61-62`), so scripted/headless capture of a chosen monitor is not expressible. Related: the X11/DRM direct path tracked in #1837.
- \[ ] **Cursor is hardcoded.** macOS forces `setShowsCursor(true)` (`screencapture.rs:66`), Linux forces `CursorMode::Embedded` (`pipewire.rs:161`), and Windows captures no cursor at all (no `PointerShape`/`PointerPosition` handling). Needs a `--cursor/--no-cursor` and a Windows implementation.

##### Device enumeration

- \[ ] **There is no way to list anything.** No public enumeration API exists in `moq-video` (`lib.rs` exports `capture`/`decode`/`encode`/`Error`) or `moq-audio`. You must already know the AVFoundation `uniqueID`, the `/dev/videoN` path, or the display index, with no way to discover them. The enumeration already happens internally and is thrown away: `SCShareableContent` (`screencapture.rs:139`), `MFEnumDeviceSources` (`mediafoundation.rs:351`), `EnumOutputs` (`desktopduplication.rs:295-302`), and cpal's `host.input_devices()` (`moq-audio/src/capture.rs:283`) each walk the list and index into it. Wants: a `list()` per source kind in the libraries, surfaced as something like `moq devices` (cameras, displays, windows, microphones), which is also the natural place to print the identifier `--camera`/`--display` expects.

##### Audio capture

- \[ ] **System / application audio loopback.** `moq-audio` capture is `Microphone` only (`capture.rs:43`); there is no loopback path anywhere in the workspace. This means `--screen` publishes a silent screen share, which is the single most surprising gap for the screen-capture use case. Needs ScreenCaptureKit audio (`SCStreamConfiguration::capturesAudio`, never set today) on macOS, WASAPI loopback on Windows, and a PipeWire monitor node on Linux.
- \[ ] **Mixing / multiple devices.** One device, one track. No mic + system-audio mix, no multi-input.
- \[ ] **Unvalidated format overrides.** `--sample-rate`-style config is applied without checking the device's supported ranges (`moq-audio/src/capture.rs:296-305`), so a bad combination fails inside cpal's `build_input_stream` instead of erroring with something actionable.
- \[ ] Opus only on the encode side (probably fine; noting it).

##### Playback (the `ffplay` equivalent)

There is **no playback surface at all**. No crate in `rs/` depends on `winit`/`wgpu`/`softbuffer`/`sdl2`/`egui`/`minifb` (zero hits across every workspace `Cargo.toml`), and there is no audio output path (cpal is used for input only; `moq-audio::AudioConsumer` decodes to PCM and stops there). The pieces that *do* exist are decode-to-raw: `moq_video::decode::Frame` (`into_i420`, `resize`) and `moq_audio::codec::Decoder::decode_f32`.

So this is a new subcommand, not a gap in an existing one. Proposed: `moq --client-connect <url> --broadcast <name> play`.

- \[ ] **Video output window** (winit + wgpu or softbuffer), fed by `moq_video::decode`. Decode is done; presentation is not.
- \[ ] **Audio output** via a cpal output stream, fed by `moq_audio::codec::Decoder`. `moq-audio`'s `capture` feature already pulls cpal, so this is mostly a `playback`/`output` sibling module.
- \[ ] **A/V sync**: present against a common clock with a latency target, reusing the `--latency-max` semantics the export sinks already have, rather than inventing a second knob.
- \[ ] **Zero-copy present**: the hardware decoders can hand back GPU surfaces; going through `into_i420` and back to the GPU wastes the whole point on 4K screen shares. Probably a follow-up, but the API shape should not preclude it.
- \[ ] **Controls**: at minimum quit; likely pause, fullscreen, and a stats overlay (bitrate / fps / latency / dropped groups), which is the thing that actually makes it useful for debugging a relay.
- \[ ] **Platform coverage** and the usual HW validation caveat from #1837: capture and presentation both need a real session, which a headless agent can't provide.
- \[ ] **Decide the surface**: `play` as its own verb vs an `export` sink. `export` is defined as "MoQ out to one sink" and a window arguably is one, but every other `export` sink writes bytes somewhere, and playback needs its own clock/controls. My read is a separate `play` verb is the honest shape; worth a decision before anyone writes code.
- \[ ] **Feature gating**: playback pulls a window system + GPU stack, which is exactly the dependency-weight problem `capture` already has. Whatever we decide above for shipping `capture` should cover this too.

##### Related, if playback lands

- \[ ] `moq-ffi` has no video decode at all (`moq-ffi/Cargo.toml` depends on `moq-audio` but not `moq-video`), so the UniFFI bindings only get raw encoded frames via `MoqFrame`. `libmoq` has `moq_consume_video_raw` (`libmoq/src/video.rs`), but it is H.264-only and its `moq_video_decoder_output` exposes only `latency_max_ms` (no format/resolution knob, per the docstring). Tracked loosely in #2152 / #2153; noting it here because a `play` implementation is the thing that would shake out the right shape.

##### Docs

- \[ ] **Keep `doc/bin/cli.md` in step with whatever lands above**, plus the Cross-Package Sync sweep for `moq-cli` examples across `doc/bin/`, `doc/lib/`, `doc/setup/`, and `doc/concept/`.

  **Correction:** this issue originally claimed the page never mentions `import capture` ("zero hits for capture"). That was wrong: the grep ran against a `main` checkout, and the capture docs live on `dev` along with the code. `dev`'s `doc/bin/cli.md` documents the `capture` subcommand, its flags, and the `--features capture` build. Apologies for the noise.

---

Dependencies: the codec/platform backends this builds on. Linux screen capture landed (`capture/pipewire.rs`); Windows capture hardware validation and the per-platform live camera runs have not. This issue covers the `moq-cli` surface: sources we can't name, devices we can't list, audio we can't capture, and playback we can't do.

## Required

- [Plan: the moq-cli capture and playback backlog](/quest/m2/plan-cli-capture.md) - split into implementable quests first
- [Video hardware validation](/quest/m3/video-hardware.md) - the Windows capture and live-camera runs this builds on have never been run on real hardware

## Closes

- [#2272](https://github.com/moq-dev/moq/issues/2272) - close this issue when the quest finishes
