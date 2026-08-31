# [L] moq-net: keyframe request (PLI equivalent) for fast tune-in

## Goal

Implement and verify the behavior tracked in [#2284](https://github.com/moq-dev/moq/issues/2284)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

A subscriber joining mid-stream waits up to a full GOP before it can decode anything  -  2 seconds by default. WebRTC solves this with a PLI: the receiver asks the sender for an IDR and gets one in ~1 RTT. MoQ has no upstream signal for it, so the only lever is a shorter GOP, which costs bitrate for every viewer to benefit the occasional joiner.

This matters most exactly where MoQ wants to win: conferencing, and any direct publisher→subscriber path with no relay cache in between.

**The encoder half is already done and tested. Only the signaling is missing.**

#### What exists today

**Group boundary == keyframe is a protocol invariant**, enforced on the write path in both languages:

- `rs/moq-mux/src/container/producer.rs:93`  -  a keyframe closes the open group and starts a new one; a non-keyframe with no open group is `MissingKeyframe` (`:109`).
- `js/hang/src/container/legacy.ts:63`  -  same, `throw new Error("must start with a keyframe")`.
- The invariant is load-bearing enough that the legacy wire format **doesn't even transmit the keyframe flag**  -  `legacy.ts:15` and `rs/moq-mux/src/codec/h264/export.rs:250` hardcode `keyframe: false` on decode and reconstruct it from group position (`export.rs:292`: "the Consumer treats the first frame of every group as keyframe by protocol invariant").

So a new subscriber waits for the next group. `encode::Config::gop` defaults to `framerate * 2` (~2s) and its doc says it plainly (`rs/moq-video/src/encode/encoder.rs:65`): "Subscribers joining mid-stream wait at most this many frames before they can start decoding." JS `keyframeInterval` defaults to 2000ms.

**Forcing an IDR works on every backend, today:**

```rust
// rs/moq-video/src/encode/encoder.rs
pub fn encode_rgba(&mut self, rgba, width, height, keyframe: bool) -> Result<Vec<Bytes>, Error>   // :155
pub fn encode_i420(&mut self, data, width, height, keyframe: bool) -> ...                          // :189
pub fn encode(&mut self, frame: &crate::decode::Frame, keyframe: bool) -> ...                      // :215
// rs/moq-video/src/encode/backend/mod.rs:40
fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;
```

`encode_rgba`'s doc even names this use case: "Set `keyframe` to force an IDR (e.g. on resume so a re-subscribing viewer can start decoding at once)."

Backends: NVENC uses the `FORCEIDR` picture flag (`backend/nvenc.rs:168`)  -  deliberately not `pictureType`, since `enablePTD` makes NVENC ignore that  -  with `repeatSPSPPS` so every IDR carries in-band parameter sets; openh264 `:46`; VAAPI `:59`; VideoToolbox and MediaFoundation the same flag. Tested: `nvenc_h264_keyframes_carry_param_sets`, `nvenc_h265_keyframes_carry_param_sets`, `nvenc_h264_periodic_idr_at_gop`.

`publish_capture` (`rs/moq-video/src/encode/producer.rs:147`) forces a keyframe on the first frame only and otherwise rides the backend's GOP cadence.

JS: `js/publish/src/video/encoder.ts:216` already calls `encoder.encode(frame, { keyFrame })`, but `lastKeyframe` is a closure-local `let` inside `#encode` with **no external trigger**. `Config.keyframeInterval` is cadence, not on-demand.

**There is no upstream request channel.** `ControlType` (`rs/moq-net/src/lite/stream.rs:9`) is fixed: `Session`, `Announce`, `Subscribe`, `Fetch`, `Probe`, `Goaway`, `Track`. The only subscriber→publisher feedback is **PROBE** (`rs/moq-net/src/lite/probe.rs:10`):

```rust
pub struct Probe { pub bitrate: u64, pub rtt: Option<u64> }
```

Two numeric fields, hard-wired to bandwidth/RTT, no extension point and no opaque payload. There is no seam for a subscriber request of any other kind.

#### The counter-argument, which needs answering first

**Behind a relay, the cache already solves this and does it better than a PLI.** The joiner doesn't need a *new* keyframe  -  the current group's keyframe is already in `cache::Pool`, and `fetch_group(latest)` retrieves it with no publisher involvement and no fan-out amplification. A relay serving 10k viewers must never forward 10k PLIs upstream; WebRTC SFUs spend real effort suppressing exactly this.

So the honest scope is narrower than "MoQ needs PLI":

- **Direct publisher↔subscriber** (conferencing, P2P-ish, no caching relay in path)  -  a PLI is straightforwardly correct here.
- **Cold start at the origin**  -  first ever subscriber, nothing cached yet.
- **Recovery** after a decoder error or an `Evicted` group.

For the relay-fanout case the answer is probably "fetch the current group, don't ask for a keyframe". Worth confirming that `fetch_group(latest)` actually gives a joiner a decodable start today  -  if it does, that's the fix for the common case and it needs documenting rather than building.

#### Proposed shape

Assuming the direct case is worth serving:

1. **An upstream request message.** Either a new `ControlType` variant or a field on the Subscribe stream. Prefer something **generic** over a PLI-specific message  -  the codebase has *no* seam for subscriber→publisher requests, and a keyframe request is unlikely to be the last one we want. But a generic escape hatch is also how protocols rot, so this needs a real decision, not a punt.
2. **Relay policy**: coalesce/rate-limit, and prefer serving from cache over forwarding. A relay must never fan a PLI storm at an origin. This is the part that determines whether the feature is safe.
3. **Rust**: a channel into the capture loop flipping the existing `keyframe` bool. No encoder work.
4. **JS**: a Signal the encode effect reads instead of the closure-local `lastKeyframe`. Small.
5. **Rate-limit at the publisher** too, independently of relays. A malicious or broken subscriber must not be able to pin the encoder at all-IDR.

#### Open questions

1. **Is the relay-cache path already sufficient for the common case?** If yes, scope this to direct sessions only and the priority drops a lot.
2. **Generic feedback channel vs. a PLI-specific message.** Leaning generic-but-typed (an enum of request kinds, not an opaque blob), so the next one doesn't need another `ControlType`.
3. **Does this deserve a wire slot at all**, versus just publishing shorter GOPs when a session is direct and interactive? A publisher that knows it's in a call could just use a 500ms GOP. Cheaper, no protocol change, worse steady-state efficiency.
4. **Interaction with `moq-transcode`**, where output groups mirror source seq 1:1  -  a forced IDR mid-GOP breaks that alignment.
5. **Alternative worth pricing**: a low-bitrate all-keyframe track for instant tune-in, then switch to the real rendition. Costs the publisher continuously instead of on demand, needs no protocol change, and composes with SVC.

#### Branch

`dev`  -  a new `ControlType` or a change to PROBE is a moq-lite wire change, needs a `Version` gate, and **must update `drafts/draft-lcurley-moq-lite.md` in the same PR**. The `moq-video`/`js/publish` trigger plumbing is additive and could land on `main` first behind an internal API.

#### Cross-package sync

`rs/moq-net` ↔ `js/net`; `rs/moq-video`, `js/publish`; `rs/moq-relay` (coalescing policy); `drafts/draft-lcurley-moq-lite.md`. Wire change → run `just test smoke-full`.

## Required

- [Plan: the keyframe request message](/quest/m2/plan-keyframe-request.md) - split into implementable quests first

## Closes

- [#2284](https://github.com/moq-dev/moq/issues/2284) - close this issue when the quest finishes
