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

**This probably explains #3517.** It used `FORGET = 0.9993`, which is NetEq's
*reorder* forget factor, on the equivalent of the underrun histogram, where
head uses `0.983`. Applied per arrival with no 500 ms resampling, that is a
memory far longer than intended, so the target barely moves. Confirm or refute
this before writing, because it decides how much of the rest was actually
wrong.

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
