# [L] Set up a local OBS from the browser with @moq/obs

## Goal

A `@moq/obs` package in `js/` lets a web page set up the OBS running on the
user's own machine through OBS's built-in obs-websocket server (protocol v5,
OBS 28+), so a page that hands out a relay URL configures OBS in one click
instead of asking the user to paste settings.

It connects with the password the user set in OBS, reports the OBS version
and whether the MoQ plugin is installed, and then:

- publishes: puts a destination in a dedicated OBS profile, either
  `moq_service` (relay URL, broadcast, token, and simulcast layers) or a
  custom RTMP(S) server and key for a stock OBS, applies low-latency encoder
  and output settings, and starts and stops the stream;
- reports stream state and live stats (bitrate, dropped and skipped frames,
  congestion) while it publishes;
- adds a MoQ Source for a broadcast to a scene.

A live OBS run, recorded in the PR, drives every path through the package,
including simulcast layers that each reach `<moq-watch>` as their own
rendition.

Non-goals: OBS on another machine (a public page can reach only loopback),
installing the plugin or enabling obs-websocket for the user, and a UI
component, since each page draws its own.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan for an integrator's dashboard and
demo page, which consume the package):

- ✅ An upstream package (rejected: each integrator keeps its own copy),
  because configuring OBS for a MoQ relay is generic; only the URLs and
  tokens are the integrator's.
- ✅ Simulcast included (rejected: a single rendition). Set
  `Stream1`/`WHIPSimulcastTotalLayers` with `moq_service`, following
  `doc/bin/obs.md`'s Simulcast renditions recipe. This package's live run
  is that recipe's first real run: fix the doc where it is wrong.
  [OBS simulcast check](/quest/m3/obs-multitrack-check.md) keeps only the
  dock's Go Live. The RTMP(S) path carries one rendition.
- ✅ The package holds the password in memory only (rejected: persisting
  it). A caller that keeps it across reloads chooses its own storage.
  Whoever holds the password controls that OBS: it can read other saved
  stream keys and add sources that read local files.
- ✅ m1 (rejected: m3), since it is additive and has a consumer waiting.

Recommended:

- Take `obs-websocket-js` at its newest version for the handshake, auth,
  and request types, rather than hand-rolling the protocol.
- Detect the plugin from `GetInputKindList` listing `moq_source`, and
  refuse OBS older than the plugin's minimum (`cpp/obs/buildspec.json`)
  when the MoQ path is asked for.
- Create the profile with `CreateProfile`, or switch to it if it exists,
  so the user's other profiles keep their service and settings. Refuse to
  switch while an output runs.
- `SetProfileParameter` writes the profile's config, which OBS reads when
  it loads the profile. Measure which settings take effect without a
  restart, reload the profile to apply the rest, and fail naming any
  setting that still needs a restart.
- A `StartStream` does not run the dock's low-latency tuning, which happens
  only on Go Live. Set what the profile exposes (no B-frames, a 2 s
  keyframe interval, CBR), and say which settings the active encoder
  ignores.
- Set only the output resolution and fps with `SetVideoSettings`, and leave
  the base canvas, which the user's scene layout depends on.
- Poll `GetStreamStatus` only while live, since obs-websocket emits no
  stats event. Follow `StreamStateChanged` for state.
- Chrome 147+ prompts before a public page opens a WebSocket to loopback.
  Report a denied prompt, an unreachable server, and a refused password as
  three distinct errors. Check Firefox and Safari, and fail loud where a
  browser refuses `ws://localhost` from an HTTPS page.
- Unit-test the protocol against a fake obs-websocket server on a
  controlled clock. For the live run, use OBS with the plugin on Linux
  (`nix develop .#obs`): publish to a local relay with three layers and
  watch three renditions, publish to an FFmpeg RTMP listener, and see a
  MoQ Source play.
- Document it next to the recipe in `doc/bin/obs.md`.

## Hosts

- linux - the live run drives a real OBS with the plugin

## Related

- [OBS simulcast check](/quest/m3/obs-multitrack-check.md) - keeps the dock's
  Go Live layers; this quest runs the obs-websocket recipe
- [MoQ in obs-studio](/quest/m5/obs-studio/README.md) - a native output keeps
  the same obs-websocket surface
