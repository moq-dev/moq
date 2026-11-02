# Studio

## Goal

A native, cross-platform streaming studio (`rs/moq-studio`) for creators and
teams who rely on OBS today, plus the same app built to wasm. It composites
cameras, screens, media, browser sources, and remote MoQ guests into scenes
on the GPU with moq-video and moq-audio, across several canvases at once
(landscape and portrait), and goes live to a MoQ relay and directly to RTMP,
RTMPS, and WHIP ingests.

Non-goals: Electron or any webview-hosted UI, a native or wasm plugin API,
and server-side fan-out to third-party ingests (a hosted service may offer
that outside this repository).

This README's own work is an end-to-end test in `rs/moq-studio`: with no
window, a scene goes live to a local relay and a local moq-rtmp listener, and
both receive it.

## Plan

Decided 2026-10-22 with the maintainer (/quest-plan):

- **Home:** `rs/moq-studio` in this repository, a binary beside moq-cli, so
  it co-evolves with moq-video, moq-audio, and the dial clients under the
  same CI.
- **UI:** egui with egui_dock. It is the only Rust toolkit with all of
  same-device zero-copy wgpu textures, multiple native windows, docking, and
  a proven wasm build. Slint has no docking and an unstable wgpu interop
  feature, gpui has no wgpu on macOS or Windows, and Tauri is a webview. The
  app owns the wgpu device.
- **Compositor:** a UI-free crate, `rs/moq-scene`: scene graph, sources,
  transforms, transitions, and canvases on wgpu. The program audio bus goes
  in moq-audio, whose only mixer today is output-only
  (`rs/moq-audio/src/playback/mixer.rs`). Both grow slice by slice with the
  studio UI that consumes them, never ahead of it.
- **Outputs:** the studio publishes to MoQ and pushes every RTMP(S) and WHIP
  destination itself. It remuxes its own broadcast through a UI-free egress
  library that a hosted service can also run server-side. There is no
  gateway service and no moq-cli dependency, because a studio user does not
  run a server. Destinations that take the same canvas and audio mix with
  identical encode settings share one encoder. The studio picks codecs per destination: Opus by default, AAC
  where an ingest requires it, from platform encoders only.
- **Extensibility:** first-party features plus browser sources (CEF
  offscreen, desktop only). A plugin API is a large surface to maintain with
  no consumer.
- **Web:** the full studio in wasm, served as a demo/web page, not a remote
  control. Its media runs on new browser backends in moq-video and moq-audio
  (getUserMedia, getDisplayMedia, WebCodecs) rather than a bridge to
  js/publish and js/watch, so one Rust media path drives both builds.
- **Guests:** guests and phone cameras join a moq-room prefix the studio
  owns, through a web join link or QR code.
- **Naming:** quests name no streaming platforms; ingest requirements are
  written as generic constraints.
- **Docs:** inline. Each quest extends `doc/bin/studio.md`.

Dropped from the first scoping round: relay fan-out ("encode once, the relay
restreams") and its upload savings. Direct push with shared encoders matches
OBS multistream, and the hosted version is product work outside this
repository.

Added 2026-10-25 (maintainer): the studio's
GPU encode and source import quests are in m1 so
moq-video's GPU encode and import API changes, mainly encoders taking a GPU
surface and zero-copy texture import, land before the cut.

## Required

- [Studio on macOS and Windows](/quest/m3/studio/app.md) - the studio runs on macOS and Windows, not only Linux
- [Viewer-side composition](/quest/m3/studio/compose/README.md) - sources as their own tracks plus a layout track, composed by viewers, with a server-composed program as the fallback
- [Undo and the scene model](/quest/m3/studio/scene-model.md) - undo history, linked and cloned sources, copy and paste, a downstream keyer, and per-item filters
- [Autosave](/quest/m3/studio/autosave.md) - every edit is written to the scene file, with no save step or quit prompt
- [Scene editing](/quest/m3/studio/scene-editing.md) - rename and reorder scenes, and resize the canvas while live through a new rendition
- [Broadcast sources play in sync](/quest/m3/studio/broadcast-sync.md) - subscribed broadcasts and guests play audio and video on the publisher's timestamps, with `moq play`'s delay
- [Wayland display and window sources](/quest/m3/studio/wayland-sources.md) - portal-picked sources that reopen without the picker
- [Live encode control](/quest/m3/studio/encode-control.md) - simulcast rungs that follow the bandwidth grant, live bitrate and resolution, keyframes on request
- [Stream health](/quest/m3/studio/health.md) - per-track delivery, viewer reports, and latency, with causes named
- [Relay failover](/quest/m3/studio/failover.md) - the same epoch on two relays, so a relay failure does not drop the stream
- [Instant replay and DVR](/quest/m3/studio/replay.md) - replays and clips from held groups, and viewer scrubbing through the archive
- [HTML/CSS overlays](/quest/m3/studio/overlays.md) - script-free HTML and CSS graphics through Blitz natively and the DOM in the browser
- [Animation and transitions](/quest/m3/studio/animation.md) - move, slide, wipe, and stinger transitions, and keyframed item properties
- [Video effects](/quest/m3/studio/effects.md) - a curated effect set with CSS meanings, and ML effects baked into their source
- [Snapping and guides](/quest/m3/studio/snapping.md) - preview moves snap to edges, centers, and other items
- [Guided setup](/quest/m3/studio/guided-setup.md) - first launch detects devices, measures the uplink, and proposes settings
- [Destinations](/quest/m3/studio/destinations.md) - RTMP, RTMPS, and WHIP push with per-destination codecs and shared encoders, on a UI-free egress library a hosted service can reuse
- [Multiple canvases](/quest/m3/studio/multi-canvas.md) - landscape and portrait canvases composite at once from shared sources
- [Guests](/quest/m3/studio/guests.md) - remote guests and phone cameras join through a moq-room link or QR code and appear as sources
- [Audio mixes](/quest/m3/studio/mixes.md) - per-output mixes, meters, monitoring, and sync offsets
- [Audio filters](/quest/m3/studio/audio-filters.md) - per-source noise suppression, gate, compressor, and limiter in moq-audio
- [Recording](/quest/m3/studio/recording.md) - each canvas's program, and optionally each source, recorded to disk alongside live outputs
- [Browser sources](/quest/m3/studio/browser-source.md) - web overlays rendered by out-of-process CEF into the compositor through shared GPU textures
- [Web studio](/quest/m3/studio/web/README.md) - the full studio in wasm on WebGPU and WebCodecs, served from demo/web
- [Virtual camera](/quest/m3/studio/vcam/README.md) - a canvas's program exposed as a system camera on Linux, Windows, and macOS
- [Signing identities are available](/quest/m3/studio/signing.md) - the Apple and Windows code-signing credentials packaging and the macOS camera need
- [Packaging](/quest/m3/studio/packaging.md) - signed installers for macOS and Windows and a Flatpak for Linux

## Related

- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - the OBS plugin stays the path for creators who stay on OBS
- [Linux capture](/quest/m2/capture-linux.md) - Wayland window, system audio, and app capture the studio's sources inherit
- [Windows capture](/quest/m2/capture-windows.md) - app capture and system audio the studio's sources inherit
