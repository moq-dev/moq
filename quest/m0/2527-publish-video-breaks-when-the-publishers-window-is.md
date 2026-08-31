# [L] Publish video breaks when the publisher's window is minimized, on every browser using the MediaStreamTrackProcessor polyfill

## Goal

Implement and verify the behavior tracked in [#2527](https://github.com/moq-dev/moq/issues/2527)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

`js/publish/src/video/polyfill.ts` produces frames by calling `requestVideoFrameCallback` on a detached `<video>` element and snapshotting it with `new VideoFrame(video, ...)`. That element stops being composited when the publisher's window is minimized, and video publishing breaks. Both browsers that take this path are affected, in two different ways:

- **Safari 26.5**: rVFC keeps firing at ~30/s but the element is paused, so the polyfill emits ~30 **pixel identical duplicate frames per second**. The encoder runs at full rate and full bitrate, the relay forwards video groups at the normal rate, and the viewer never enters a stalled state. The viewer just sees a still image while audio continues. Nothing anywhere reports a problem.
- **Firefox 152.0.6**: rVFC stops firing altogether, so capture starves. Encoded output drops to 0 fps, the relay forwards 0 video groups per second, and the viewer sits in the buffering state.

Chrome is unaffected because it has native `MediaStreamTrackProcessor` and never touches a `<video>` element.

Restoring the window recovers within about a second in all cases.

#### Reproduce

1. `just dev`
2. Open `publish.html` in Safari (or Firefox), start the camera, open `watch.html` in a second window.
3. Leave both windows fully visible and confirm video is live.
4. Minimize the publish window.
5. Safari: the viewer's picture freezes, audio keeps playing, and no buffering spinner appears. Firefox: the picture freezes and the buffering spinner does appear.
6. Restore the publish window. Video resumes on its own.

#### Environment

| | |
|---|---|
| OS | macOS 26.5.1 (25F80), Apple Silicon (arm64) |
| Safari | 26.5 |
| Firefox | 152.0.6 |
| Chrome | 150.0.7871.184 |
| moq | `46f1de8d` on `dev` |
| Relay | local `moq-relay` with `demo/relay/localhost.toml`, default `[::]:4443` |
| Pages | `demo/web` on `http://localhost:5174`, publisher and viewer both local |
| Transport | Safari and Firefox on WebSocket/qmux (WebTransport is disabled on Safari since #2417, and Firefox has none). Chrome on WebTransport. |
| Camera | Lenovo FHD Webcam (UVC), capture requested at the 1280x720 default |
| Encoder | all demo encoder settings left on "auto", so no explicit `frameRate` and the default 2s keyframe interval |

Both publisher and viewer windows were fully visible and side by side before the minimize, so the viewer's own `intersecting && !document.hidden` gate was never a factor. The viewer reported `document.hidden === false` for every sample in every phase.

#### Mechanism

Neither Safari nor Firefox has `MediaStreamTrackProcessor`, so `TrackProcessor()` takes the fallback. Verified at runtime in both: `self.MediaStreamTrackProcessor` is `undefined`.

The fallback creates a `<video>` that is never appended to the DOM, calls `play()` once in `start()`, and then in `pull()` awaits `video.requestVideoFrameCallback` and builds each frame as `new VideoFrame(video, ...)`. Two properties of that design are what break:

1. Frame production is driven by a **compositor callback**, so it inherits whatever the browser does to a window that is not on screen.
2. The pixel source is "whatever the element is currently presenting", and nothing verifies that it advanced.

Measured behaviour while minimized:

| | Safari 26.5 | Firefox 152.0.6 |
|---|---|---|
| rVFC calls/s | 29.9 (unchanged) | 0.0 |
| `video.paused` | **true** | false |
| `video.currentTime` rate | 0.0 | 0.0 |
| `metadata.presentedFrames`/s | 30.3 (still advancing) | 0.0 |
| result | 30 duplicate frames/s | no frames |

Safari is the more dangerous case: it pauses the element but keeps invoking rVFC *and* keeps advancing `metadata.presentedFrames`, so the callback metadata claims new frames are being presented when they are not. `video.paused` and `video.currentTime` are the only fields that reflect reality.

Downstream, nothing can detect the Safari case:

- `js/publish/src/video/encoder.ts` paces on timestamps only, and only when an explicit `frameRate` is configured, so every duplicate gets encoded.
- The encoder runs `latencyMode: "realtime"` against a target bitrate, so it spends the **full** bitrate on the static image. There is no bitrate drop to notice.
- `js/hang/src/container/legacy.ts` cuts groups on the keyframe flag, which still fires on the normal 2s cadence, so the group rate is unchanged.
- On the viewer, frames keep arriving, so `Video.Decoder`'s `out.stalled` never trips and `<moq-watch-ui>`'s buffering indicator never appears.

The camera track stays `live` and unmuted throughout in every arm.

#### Measurements

Instrumented build sampling twice a second. "uniq" counts distinct 32x32 pixel checksums of the frames leaving capture (`Capture.out.frame`), "mad" is the mean absolute pixel difference between consecutive samples. Both are measured on the publisher, on the frames going **into** the encoder. Relay counts come from `serving group` lines sliced by byte offset per phase.

##### Safari publisher, 120s minimized (viewer: Safari)

| phase | samples | uniq | mad | encoded fps | encoded kbps | relay video groups/s | viewer stalled |
|---|---|---|---|---|---|---|---|
| baseline 30s | 60 | 60 | live | 30.3 | 1842 | 0.50 | false |
| **minimized 119s** | 120 | **1** | **0.00** | **29.9** | **1835** | **0.49** | **false** |
| restored 31s | 62 | 62 | live | 30.0 | 1822 | 0.52 | false |

Content froze 0.8s after the minimize and resumed 0.8s after the restore. Across the frozen window the encoder produced **3,558 frames of one identical image**. Frame timestamps advanced normally throughout (1,000,269 microseconds per second of wall clock), so these are genuinely new frames carrying duplicate content, not a stalled signal. Audio groups stayed at 50/s in all three phases.

Repeated with a **Chrome** viewer for 70s: identical publisher result (uniq 1, mad 0.00, 29.8 fps, 1839 kbps) and the Chrome viewer also never stalled, with `document.hidden === false` throughout. So this is not a Safari viewer problem.

##### Firefox publisher, 60s minimized (viewer: Chrome)

| phase | samples | uniq | mad | encoded fps | encoded kbps | relay video groups/s | viewer stalled |
|---|---|---|---|---|---|---|---|
| baseline 35s | 70 | 68 | 5.75 | 29.8 | 1905 | 0.51 | false |
| **minimized 60s** | 119 | **1** | **0.00** | **0.0** | **0** | **0.00** | **true** |
| restored 16s | 33 | 32 | 5.13 | 29.1 | 1861 | 0.50 | false |

A second Firefox run showed the same shape with a few stragglers getting through (0.1 rVFC calls/s, 0.1 encoded fps, 5 kbps). Audio groups stayed at 50/s throughout, so only video is affected.

Note the probe's own `setInterval` kept sampling at the full 2/s while minimized in Firefox, so the page's timers were not throttled. Only rVFC delivery stopped. (Safari did throttle the probe interval to 1/s, which does not affect the rates above since they are computed from counter deltas.)

##### Control: Chrome publisher, same minimize (70s, viewer: Safari)

Chrome takes the native `MediaStreamTrackProcessor` path (`hasNative === true`), so no `<video>` element is involved:

| phase | samples | uniq | mad | encoded fps | document.hidden |
|---|---|---|---|---|---|
| baseline | 50 | 50 | 8.67 | 30.3 | false |
| **minimized** | 70 | **70** | **8.32** | 30.3 | **true** |
| restored | 52 | 52 | 8.32 | 30.3 | false |

Same minimize, same measurement, content keeps changing. The only difference between the arms is which branch of `TrackProcessor()` is taken.

#### Summary of arms

| publisher | capture path | minimized: distinct frames | encoded fps | relay video groups/s | viewer stalled |
|---|---|---|---|---|---|
| Safari 26.5 | polyfill (`<video>` + rVFC) | 1 | 29.9 | 0.49 | false |
| Firefox 152.0.6 | polyfill (`<video>` + rVFC) | 1 | 0.0 | 0.00 | true |
| Chrome | native `MediaStreamTrackProcessor` | 70 of 70 | 30.3 | 0.51 | false |

#### Notes

Audio is unaffected in every arm because publish audio runs on an `AudioWorkletProcessor` (`js/publish/src/audio/capture-worklet.ts`) on the real time audio thread, which minimizing does not touch. That asymmetry is what makes the Safari case look like "video froze but the stream is fine".

The Safari variant is invisible to every health signal the stack currently has: encoded frame count, encoded bitrate, group rate, relay throughput, and viewer stall state all look completely normal. The only way we could detect it was by checksumming the pixels entering the encoder.

There is arguably also a WebKit bug in reporting `metadata.presentedFrames` as advancing for a paused element whose `currentTime` is frozen, which rules out `presentedFrames` as a way to detect the condition.

Raw beacon files, relay logs, and the analysis scripts for all five runs are available if useful.

## Closes

- [#2527](https://github.com/moq-dev/moq/issues/2527) - close this issue when the quest finishes
