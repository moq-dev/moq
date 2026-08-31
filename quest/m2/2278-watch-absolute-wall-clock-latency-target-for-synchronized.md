# [L] watch: absolute wall-clock latency target for synchronized playback across viewers

## Goal

Implement and verify the behavior tracked in [#2278](https://github.com/moq-dev/moq/issues/2278)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Let every viewer render the same frame at the same instant. Watch parties, sports bars, betting, second-screen, and multi-camera sync all need it. HLS does it badly via `EXT-X-PROGRAM-DATE-TIME`; WebRTC doesn't try.

The pieces are closer than they look: **the PTS → wall-clock mapping already exists in the catalog and the player just never reads it.**

#### What exists today

**`Timeline.wall` is the mapping, and it's already shipping.** `rs/hang/src/catalog/timeline.rs`:

```rust
pub struct Timeline {
    pub track: String,
    pub timescale: u32,          // default 1000 (ms)
    pub wall: Option<u64>,       // wall-clock of pts 0, in timescale units since the MoQ epoch
}
pub const MOQ_EPOCH_UNIX_MILLIS: u64 = 1_577_836_800_000;  // 2020-01-01
```

Set via `moq_mux::timeline::Producer::set_wall(pts, wall)` (`rs/moq-mux/src/timeline.rs`), attached per-rendition on `VideoConfig.timeline` / `AudioConfig.timeline`. JS mirrors the schema (`js/hang/src/catalog/timeline.ts`) and the producer (`js/hang/src/container/timeline.ts`  -  `setWall(pts, wall)`).

So `wall + pts` → Unix time, per rendition, today.

**Frame timestamps are PTS, deliberately, and must not be repurposed.** Worth stating clearly because it's easy to get wrong:

- `hang::container::Frame::timestamp` doc (`rs/hang/src/container/frame.rs:27`): "the presentation timestamp... This is NOT a wall clock time."
- `rs/moq-net/src/model/time.rs:119`: "All timestamps within a track are relative, so zero for one track is not zero for another."
- `Timestamp::now()` is a deliberately one-way bridge with **no inverse**, and the anchor is 2020-01-01 **minus a per-process random 0..69420ms jitter**, explicitly "to deter nerds trying to use timestamp as wall clock time" (`time.rs:412`).
- The lite-05 wire field is a **zigzag delta of PTS at the track timescale** (`rs/moq-net/src/lite/publisher.rs:828` `encode_frame_timing`), not a wall-clock value.

The codebase actively fights wall-clock interpretation of frame timestamps. `Timeline.wall` is the sanctioned channel and this issue should stay on it.

#### What's missing

**Latency is relative (buffer depth), never absolute.** `js/watch/src/sync.ts`:

```ts
export type Bound = "real-time" | Time.Milli;
export type Latency = Bound | { min?: Bound; max?: Bound };
```

`Sync.received(timestamp)` computes `ref = Time.Milli.now() - timestamp`, and `Time.Milli.now()` is `performance.now()` (`js/net/src/time.ts:68`)  -  monotonic since page load, explicitly "not wall-clock time". So `reference` is an arbitrary local-clock↔PTS offset anchored by whichever frame happened to arrive first. Two viewers who tune in 3 seconds apart are 3 seconds apart forever, and nothing in the model can tell.

`Sync` never reads `Timeline.wall`. There is no JS `Timeline.Consumer` at all (producer only).

#### Proposed shape

1. **A JS `Timeline.Consumer`** in `js/hang` (needed by the DVR work too).
2. **A third `Bound` variant, or a new absolute mode**: something like `latency: { absolute: Time.Milli }` meaning "render pts P at wall time `wall + P + absolute`". `Sync` then anchors on `Timeline.wall` instead of first-frame arrival, and `now()` becomes a wall-clock computation rather than an offset.
3. **Client clock sync.** This is the real work. `Timeline.wall` is the *publisher's* wall clock; the viewer's `Date.now()` can be off by seconds. Options, roughly in order of cost:
   - trust `Date.now()` (fine for a watch party, useless for betting)
   - estimate offset from the session RTT (we already track min RTT via PROBE in `#runJitter` for exactly this kind of anchoring)
   - NTP-ish round trips over a MoQ track
   - let the app inject a clock
     Suggest: an injectable clock with an RTT-based default, so the app can bring its own.
4. **Degrade honestly.** If the target wall time has already passed (viewer joined late, or clock skew is large) the player must either skip forward or admit it can't hit the target. Silently drifting back to relative is worse than an error.
5. **Surface the achieved offset** so an app can show "you are 240ms behind the reference".

#### Open questions

- Absolute latency and the existing `min`/`max` bounds interact awkwardly: an absolute target is a *point*, the bounds are a *range*. Does absolute override the range, or clamp within it?
- `Timeline.wall` is per-rendition. Are audio and video guaranteed consistent? A rendition switch must not re-anchor.
- Does this need anything on the publisher, or is `set_wall` sufficient? Today nothing in `moq-video`/`js/publish` calls `set_wall` as far as I can tell  -  so step 0 might be "actually populate `wall`".

#### Naming note

`Latency` is already doubly overloaded: the `Bound | {min,max}` type (`js/watch/src/sync.ts:16`) and `class Latency` (`js/hang/src/util/latency.ts:21`, jitter+buffer). Different packages, both exported. Worth resolving before adding a third meaning.

#### Branch

`main`  -  additive on the player. Unless `Timeline` gains fields, in which case check `#[non_exhaustive]` (it has it) and the JS schema.

#### Cross-package sync

`js/watch`, `js/hang`, `rs/hang`; `demo/web` if it exposes the knob; `doc/concept`.

## Closes

- [#2278](https://github.com/moq-dev/moq/issues/2278) - close this issue when the quest finishes
