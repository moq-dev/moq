# [S] The audio playout target algorithm is written down once

## Goal

One document under `doc/concept` describes how a receiver estimates its audio
playout target from arrival timing, in enough detail that two implementations
written from it agree on the same trace. Every constant has a stated origin. A
conformance corpus, arrival traces and the target series each must produce,
lives beside it as checked-in JSON so both languages test against the same
numbers rather than against each other.

Boundaries: no production code. The implementations are the sibling quests, and
either may send a correction back here.

## Plan

The survey is done; it is recorded here so the implementations do not repeat
it. What remains is the writing and the corpus.

### What to implement against

WebRTC's NetEq, but not the file the name suggests. `delay_manager.cc` on
current main is a 103-line composition; the algorithm lives in
`underrun_optimizer.{h,cc}`, `reorder_optimizer.{h,cc}`, `histogram.{h,cc}`,
and `packet_arrival_history.{h,cc}` under
`modules/audio_coding/neteq/`. In priority order:

1. **Relative arrival delay**, the highest-value idea and the cheapest.
   `delay(p) = max(0, (arrival(p) - arrival(ref)) - (rtp(p) - rtp(ref)))` where
   `ref` is the packet in a 2 s window minimising `arrival - rtp`, that is, the
   fastest recent packet, tracked with a monotonic deque. Delay against the
   best-case path, not against the previous packet. An inter-arrival estimator
   is structurally blind to a slow build-up: a hundred packets each 2 ms late
   read as a hundred tiny observations, where this reads as a delay climbing to
   200 ms. Frame timestamps stand in for RTP timestamps and there are no
   sequence numbers to unwrap.
2. **An empirical histogram with an exponential forget factor, read at a
   quantile.** 20 ms buckets, 100 of them. Seed the cold start with the
   decaying `0.5^(i+1)` prior. A quantile of an empirical distribution, not a
   mean plus k deviations: network delay is heavy-tailed and one-sided, so a
   Gaussian assumption sizes the tail wrong.
3. **Max-over-interval resampling**, 500 ms. Easy to miss and behaviourally
   significant: the histogram takes one observation per interval, the maximum
   in it. That decorrelates observations and turns the forget factor into a
   wall-clock time constant, which is the only form worth documenting.
4. **A cold-start forget ramp**, WebRTC's `start_forget_weight`, so the first
   seconds converge instead of crawling.
5. **Explicit delay-versus-loss cost minimisation** for the late tail. WebRTC's
   `ReorderOptimizer` and SpeexDSP's `compute_opt_delay` reached the same shape
   independently, which is decent evidence it is right. Defer it; ship the
   underrun estimator first.

Current defaults on main, for reference and not for copying blind: quantile
`0.95`, underrun forget factor `0.983`, resample interval `500ms`, start
forget weight `2`, reorder forget factor `0.9993`, `ms_per_loss_percent` `20`.
The widely repeated "0.97" is a transitional value, not head.

Say explicitly that the forget factor decays once per *resampled observation*,
not once per arrival, and give the resulting wall-clock memory
(`500ms / (1 - 0.983)`, about 29 s). Applying a per-observation factor per
arrival is precisely the mistake #3517 made, and a number that reads plausible
either way is exactly the kind that survives review.

Define units once, for both the algorithm and the corpus: what clock arrival
times are on, what timescale frame timestamps use and how they convert, and
what a timestamp discontinuity does to the reference. Two implementations that
disagree on any of these produce different targets from the same trace while
both look correct.

**What actually breaks #3517, reproduced.** A viewer on that branch reads a
14.56 s jitter buffer in auto. The estimator cannot produce that from its
histogram: the percentile is clamped to `BUCKETS * BUCKET`, one second. The
unbounded term is the "plus one frame" on top of it.

`Jitter` learns that frame duration as the running minimum of positive gaps
between consecutive observed media timestamps, so the *first* gap sets it with
nothing to validate against. A tune-in that observes a frame from a stale group
and then the live edge sets it to the distance between them, seconds rather
than milliseconds. `#publish` then raises the target instantly and
unconditionally to `percentile() + step`, and lowers it by one *corrected*
step per second afterwards. Inflation is immediate; deflation is 20 ms/s.
`reanchor()` clears `#latest` but not the spacing, so the bad value outlives
the discontinuity that caused it.

Replaying two frames 14.5 s apart in media time, then twenty minutes of
perfectly paced audio with zero real jitter:

```
after 2 frames: spacing=14500ms  target=14.50s
  +   1s live, zero jitter: target=14.48s
  +  60s live, zero jitter: target=13.30s
  + 300s live, zero jitter: target=8.50s
  + 600s live, zero jitter: target=2.50s
  + 900s live, zero jitter: target=0.02s
```

Two lessons for the document, both structural rather than a constant to retune:

- **Frame duration is a codec property, not an observation.** NetEq's
  packet-time units come from the codec for the same reason. Deriving it from
  arbitrary timestamp arithmetic makes a tune-in artifact indistinguishable
  from a real frame duration.

  Where it comes from is an open question this quest answers, because the
  catalog does not carry it: `AudioConfig` has `sampleRate` and `bitrate`, and
  its own comment concedes the frame duration "depends on the codec, sample
  rate, etc." Opus makes this sharp, since the encoder picks 2.5, 5, 10, 20,
  40, or 60 ms independently of both. Candidates, in order of how little they
  cost: the Opus TOC byte, which self-describes the frame duration in band and
  needs no schema change; the decoded frame's own sample count, exact but only
  known below the container layer where the estimator currently sits; or a new
  catalog field, which is the expensive option and would need the full
  cross-package sync (`rs/hang`, `js/hang`, `doc/concept`, and the hang draft).
  Pick one and say why; do not leave it to each implementation.
- **The target needs a clamp and a bounded rise.** WebRTC clamps in
  `DelayConstraints` (`delay_constraints.{h,cc}`), applied by `DecisionLogic`
  rather than by the delay manager, with a hard ceiling at 75% of buffer
  capacity. #3517 ported neither the clamp nor a rise limit, so any single bad
  observation is permanent on the timescale a viewer will wait.

The forget factor is a separate and smaller divergence: `0.9993` is NetEq's
*reorder* factor where the underrun path uses `0.983`, applied per arrival with
no 500 ms resampling. Worth fixing, but it is not what produces 14.56 s.

### Licensing

WebRTC is BSD-3 plus a PATENTS grant. Copying source means carrying a third
license and its disclaimer inside a `MIT OR Apache-2.0` repository. Implement
from the described algorithm instead, in `f64`, citing WebRTC in a comment as
the design's source. The Q15/Q30 fixed point exists for 2010-era DSPs; floating
point is simpler here and removes a whole class of cross-language parity bug.

Do not read Asterisk's `main/jitterbuf.c` (GPLv2) or FreeSWITCH's
`switch_jitterbuffer.c` (MPL 1.1) while writing this. SpeexDSP's `jitter.c` is
BSD-3 and safe to read for the cost-minimisation idea.

### Why not a dependency

The `neteq` crate (`security-union/videocall-rs`, MIT OR Apache-2.0, actively
released, ships a WASM build) is the only viable candidate and was rejected:
its relative-arrival-delay step takes a percentile over per-packet deltas each
clamped at zero, which cannot see a build-up, the one property the design
exists for; its `ReorderOptimizer` is configured but never implemented; and it
is a whole NetEq, sample-rate-locked ring buffers and WSOLA and PLC, where this
needs a few hundred lines of estimator. Its shape would leak into our API.
Nothing else is close: `webrtc-rs` and `str0m` deliberately leave playout to
the application, GStreamer's `rtpjitterbuffer` solves clock skew rather than
target sizing and is LGPL, and no browser-usable JS port exists.

### The document

Write `doc/concept/`, covering the observation point, the estimator, rise and
fall, startup and re-anchoring, and the clamp. Specifically:

- Measure at container frame arrival and before the age budget can skip a
  group. #3517 has this right, and it matters: a target derived from what
  survives the budget only ever confirms the budget it was cut to.
- Rise and fall asymmetry is the "too aggressive or not aggressive enough"
  dial, so it gets a justification rather than a number.
- The clamp reads the rendition's advertised flush span from [Jitter
  estimator](/quest/m0/3479-mux-jitter-flush-span.md). That field is a
  publisher-declared floor and the estimate is a measurement of the network:
  say plainly which is which, because the shared word "jitter" invites
  confusion. #3517 combines them with a max on the grounds they describe the
  same quantity; that reasoning does not survive the distinction, so revisit it.
- The document is normative, not either implementation. Parity is held by the
  conformance corpus, one JSON fixture set run by both test suites, the way
  wire behaviour is held aligned today. A wasm core shared into the browser
  audio path is a larger architectural commitment than this warrants.

Not an IETF draft: nothing here is negotiated on the wire, and a receiver that
chooses differently is still conformant.

## Related

- [Jitter estimator](/quest/m0/3479-mux-jitter-flush-span.md) - supplies the advertised flush span the clamp reads
