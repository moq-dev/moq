# [M] moq-ffi 0.2.27+: MoqMediaConsumer returns MoqFrame.timestampUs=0 for all frames after…

## Goal

Implement and verify the behavior tracked in [#2143](https://github.com/moq-dev/moq/issues/2143)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

````markdown
# `moq-ffi 0.2.27+`: `MoqMediaConsumer` returns `MoqFrame.timestampUs=0` for all frames after timeline-track migration (PR #2109)

## Environment

- **moq-ffi version:** `0.2.28` (regression first observed in `0.2.27`)
- **Last working version:** `0.2.26`
- **moq-relay version:** `0.13.3`
- **Client:** Kotlin Android app via UniFFI bindings
- **Protocol negotiated:** `moq-lite-04` (relay also supports `moq-lite-05`, but our stack currently negotiates `-04` with `0.2.26`)

## What works vs. what is broken

| Component | Status | Notes |
|-----------|--------|-------|
| Web viewer (`@moq/net` 0.1.7) | Works | Reads timeline tracks (`0.hev1.timeline.z`) and rehydrates timestamps |
| moq-relay `0.13.3` | Works | Forwards both media and timeline tracks correctly |
| Android publisher (`moq-ffi 0.2.26`) | Works | Publishes media frames with inline timestamps |
| Android viewer (`moq-ffi 0.2.28`) | **Broken** | `MoqMediaConsumer` returns `timestampUs=0` for every frame |
| Android viewer (`moq-ffi 0.2.26`) | Works | Timestamps are present and playback is normal |

## Why moq-lite-04 and not moq-lite-05

`moq-ffi 0.2.26` negotiates `moq-lite-04` with our relay (`moq-relay 0.13.3`). Upgrading to `moq-ffi 0.2.28` shifts the protocol preference to `moq-lite-05`, but the protocol version itself is not the cause of the bug. The regression is caused by the **container format change** introduced in PR #2109, which moved frame timestamps from the media frame into a separate timeline track.

Both protocol versions are capable of carrying timeline tracks, but the Android `MoqMediaConsumer` does not consume them in either case.

## What changed in 0.2.28

Release `moq-ffi v0.2.28` includes PR #2109  -  "Per-track timeline index for each media track". Frame timestamps moved from inline media frames into separate timeline tracks (e.g. `0.hev1.timeline.z`, `0.opus.timeline.z`).

The catalog now advertises:

```json
{
  "codec": "opus",
  "sampleRate": 48000,
  "numberOfChannels": 1,
  "container": { "kind": "legacy" },
  "timeline": { "track": "0.opus.timeline.z", "timescale": 1000 }
}
````

#### Expected behavior

`MoqMediaConsumer.next()` should return `MoqFrame` objects with meaningful `timestampUs` values, so the client can buffer and schedule playback correctly.

#### Actual behavior

Every frame consumed through `MoqMediaConsumer` has `timestampUs == 0`.

#### Impact

The Android video pipeline never leaves the `BUFFERING` state. The jitter buffer requires:

```kotlin
newest - oldest >= targetBuffering
```

With all frames at `pts=0`, the depth is always `0`, so no frames are ever forwarded to the MediaCodec decoder. The user sees a black screen while audio continues to play.

#### Evidence

Diagnostic logging from the Android consumer:

```text
video frame#1  pts=0us key=true  bytes=28425 bufState=BUFFERING bufDepth=0.0ms
video frame#2  pts=0us key=false bytes=5791  bufState=BUFFERING bufDepth=0.0ms
video frame#90  pts=0us key=false bytes=3926  bufState=BUFFERING bufDepth=0.0ms
video frame#180 pts=0us key=false bytes=2788  bufState=BUFFERING bufDepth=0.0ms
```

#### Root cause

The `MoqMediaConsumer` UniFFI path does not subscribe to the per-track timeline track, nor does it rehydrate frame timestamps from it. The `MoqFrame` schema is unchanged, but the `timestampUs` field is now empty because the timestamps were moved out of the media frame.

#### Why this cannot be fixed in client Kotlin

The `MoqMediaConsumer` API is unchanged between `0.2.26` and `0.2.28`:

```kotlin
subscribeMedia(name: String, container: Container, maxLatencyMs: ULong): MoqMediaConsumer
MoqMediaConsumer.next(): MoqFrame
MoqFrame { payload: ByteArray, timestampUs: Long, keyframe: Boolean }
```

The timeline track is not exposed to the caller, so the client cannot read timestamps itself. The fix belongs in the Rust `moq-ffi` layer.

Also note: `software-mansion-labs/moq-kit` is still pinned to `moq-ffi 0.2.25`, so no higher-level Android/iOS SDK has yet been ported to the timeline API.

#### Current workaround

Pin `moq-ffi` to `0.2.26`. This restores inline timestamps and normal video playback.

#### What needs to be done upstream

Please implement one of the following:

1. **Restore inline timestamps for the legacy consumer path**  -  make `MoqMediaConsumer` internally join the timeline track and populate `MoqFrame.timestampUs` before returning frames to callers.
2. **Expose the timeline track through the FFI**  -  provide a way for callers to subscribe to `*.timeline.z` and correlate timeline entries with media frames.
3. **Document the breaking change**  -  if `timestampUs` is intentionally no longer available via `MoqMediaConsumer`, update the FFI API and migration guide so client maintainers know how to adapt.

#### Labels

`moq-ffi`, `timeline`, `breaking change`, `media consumer`

```

## Closes

- [#2143](https://github.com/moq-dev/moq/issues/2143) - close this issue when the quest finishes
```
