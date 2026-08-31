# [M] Publish files by demuxing and decoding them, not by capturing a MediaStreamTrack

## Goal

Implement and verify the behavior tracked in [#2532](https://github.com/moq-dev/moq/issues/2532)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

`js/publish/src/source/file.ts` publishes a local file by playing it in a `<video>` element and turning that back into a live capture track. On Chrome and Firefox it uses `HTMLMediaElement.captureStream`; WebKit doesn't implement that, so Safari draws each frame onto a canvas at 30fps and uses `canvas.captureStream(30)`. Images take the canvas path on every browser.

Publishing a file is a transcode, not a capture, and forcing it through a capture pipe is what causes the problems below. We should demux the file and drive `VideoDecoder`/`AudioDecoder` directly, producing `VideoFrame`s with the container's own timestamps and no media element in the loop.

#### Motivation

Concrete things the current shape costs us, roughly in order of severity:

- **No audio on Safari, at all.** `#decodeMedia` logs a warning and publishes video-only, because WebKit exposes a media element's audio only through Web Audio and only once microphone permission is granted. An audio-only file fails outright with a "this browser can't capture audio from files" error.
- **No usable timestamps on Safari.** WebKit reports `timestamp: 0` for every frame off a canvas capture track. Measured in Safari 26.5, eight consecutive frames from `canvas.captureStream(30)` all carried `ts=0` while arriving on a correct ~36ms cadence. #2528 works around this by falling back to arrival time when the source clock never advances, which restores parity but throws away the file's real presentation timestamps.
- **Frozen when the window isn't composited.** The canvas is redrawn by an `effect.interval` draw loop off a `<video>` element, so the frames stop changing when the publisher's window is minimized. This is the file-source half of #2527, and the worker pipeline from #2224 can't fix it: the track itself stops receiving new pictures. A decoder driven by a worker-side pacing loop is not compositor-bound.
- **A wasted full-frame blit per frame** on the canvas path, plus a decode the media element already did.
- **Playback rate is whatever the element does.** Frames are sampled at a fixed 30fps regardless of the file's real frame rate, so a 24fps or 60fps source is resampled by luck of the draw.

#### Sketch

- Demux the file into `EncodedVideoChunk`/`EncodedAudioChunk`. `js/hang/src/container/cmaf/decode.ts` only parses the CMAF we produce ourselves, not an arbitrary user-picked MP4/MOV/MKV, so this needs a real demuxer: mp4box.js, or libav.js which is already in the dependency tree via `@kixelated/libavjs-webcodecs-polyfill` (see `js/hang/src/util/libav.ts`).
- Feed the chunks to `VideoDecoder`/`AudioDecoder` and pace the output against a wall clock, since decode runs far faster than realtime. Looping means restarting the demuxer and offsetting timestamps by the file duration.
- Prerequisite shared with any non-track source: `Video.Source` is currently `StreamTrack`, so it has to widen to accept a stream of frames. That is the same prerequisite as feeding `<video>` frames directly via `requestVideoFrameCallback`, which is a reasonable intermediate step if this lands in stages.
- Keep the media-element path around only if it turns out to be worth it; ideally one code path serves every browser.

#### Notes

Follow-up to #2224 and #2528. The workaround in #2528 keeps the file source working on Safari but does not make it good; this issue is the actual fix. The minimized-window behavior of the file source is out of scope for #2527, which is about camera capture.

## Closes

- [#2532](https://github.com/moq-dev/moq/issues/2532) - close this issue when the quest finishes
