---
title: Audio jitter
description: How a receiver sizes its audio playout target from arrival timing
---

# Audio jitter

A receiver cannot play audio the instant it arrives. Frames leave the publisher
in bursts and cross a network that delivers them unevenly, so a playhead that
starts on the first frame runs dry on the first gap. The buffer it holds ahead
of the playhead is the **playout target**, and this page says how to compute it.

This document is normative. `@moq/hang` and `moq-audio` each implement it, and
neither is the reference for the other: parity is held by the [conformance
corpus](#conformance-corpus), one set of arrival traces with the target series
both must produce. Nothing here is negotiated on the wire, so a receiver that
chooses a different target is still conformant. It will just sound worse.

The target is a *measurement*, not a round trip. `max(20ms, 1.25 x minRtt)`
sizes a buffer for one retransmit, which describes how long the network takes
to recover a loss and says nothing about how unevenly a publisher emits frames.
A sender flushing 100 ms of media at once needs 100 ms of buffer on a perfect
network, and the round trip cannot see it.

## The observation point

Every frame is observed **as it comes off the transport, before any group can be
skipped**. A target derived from what survives the age budget only ever confirms
the budget it was cut to.

That point is one await in each implementation, resolving once per container
frame:

| | Observation point |
| --- | --- |
| Browser | `Group.Consumer.readFrame()`, inside the container consumer's per-group loop |
| Native | `container::Consumer::read()`, awaited by `moq-audio`'s decode consumer |

Both sit below the decoder and above nothing: no reordering, no gap handling, no
age budget. A frame dropped by the relay is never observed, which is correct.
A frame the container will later discard *is* observed, which is also correct:
it arrived, and when it arrived is the measurement.

## Units and clocks

Two implementations that disagree here produce different targets from the same
trace while both look right, so this is fixed rather than left to taste.

- **Arrival** is the receiver's monotonic clock in **milliseconds**, as `f64`.
  `performance.now()` in the browser, `Instant` natively. Never the wall clock:
  a step adjustment would read as a burst of jitter.
- **Media** is the frame's container timestamp converted to **milliseconds**, as
  `f64`, using the track's declared timescale. Convert once, at the observation
  point, as `ticks * 1000 / timescale` in `f64`. The legacy hang container is
  microseconds; CMAF and Matroska carry their own scale.
- **Every value below is `f64` milliseconds.** No fixed point and no integer
  rounding. WebRTC's Q15 arithmetic exists for 2010-era DSPs and buys nothing
  here except a class of cross-language parity bug.

Arithmetic is IEEE-754 and evaluated in the order written, so both languages get
bit-identical results. Do not reassociate the sums, do not use compensated
summation, and do not accumulate in a wider type.

## The estimator

```
observe(arrival, media):
    if media <= newest: return          # reordered, excluded entirely
    newest = media

    drop history entries with entry.media < media - WINDOW
    push (arrival, media) onto history

    ref = entry in history minimising (entry.arrival - entry.media)
    delay = max(0, (arrival - ref.arrival) - (media - ref.media))

    if first observation:
        origin = arrival; interval = 0; peak = delay; return

    index = floor((arrival - origin) / RESAMPLE)
    if index <= interval:
        peak = max(peak, delay); return

    fold(bucket of peak)
    fold(nothing) for each elapsed empty interval, at most IDLE_MAX
    interval = index; peak = delay

fold(bucket):
    count += 1
    forget = min(FORGET, count / (count + RAMP))
    histogram = forget * histogram + (1 - forget) * (bucket or PRIOR)

target():
    measured = upper edge of the first bucket whose cumulative weight exceeds QUANTILE
    return max(measured, advertised) + frame
```

### Relative arrival delay

`delay` is measured against the **fastest frame in the recent window**, not
against the previous frame. This is the whole design in one line, and it is what
an inter-arrival estimator structurally cannot do: a queue filling at 2 ms per
frame reads as a hundred tiny inter-arrival observations, where this reads as a
delay climbing to 200 ms.

The window is **media time**, not arrival time. Pruning `entry.media < media -
WINDOW` is what makes a timestamp jump self-correcting: everything before it
leaves the window at once, the new frame becomes its own reference, and the
delay measured across the jump is zero rather than the size of the jump. A
tune-in that reads a frame from a stale group and then the live edge needs no
special case, and neither does a group the relay dropped.

`WINDOW` is also the horizon over which a build-up is visible. A queue that
fills over ten seconds is measured as the part of it that happened in the last
two.

### Reordered arrivals are excluded, not measured

A frame whose media timestamp is not newer than the newest already observed is
**reordered**, and it is dropped: it updates no state, joins no history, and
produces no delay.

This is not a refinement. When a newer group arrives before an older one,
`arrival - ref.arrival` is positive while `media - ref.media` is negative, so the
media-time gap between them is *added* to the delay. That is exactly the failure
this whole design exists to avoid, a media-time distance leaking into a delay
measurement, and it arrives through the front door: the container consumer takes
groups off the transport in delivery order and sorts afterwards, so out-of-order
observation is ordinary input rather than an edge case.

Reordering is a real cost, and a receiver that wanted to price it would weigh
late arrivals against the loss of playing without them. That is WebRTC's
`ReorderOptimizer` and SpeexDSP's `compute_opt_delay`, which reached the same
shape independently. It is deliberately out of scope here: ship the underrun
estimator first.

### One observation per interval, the maximum

The histogram takes **one observation per `RESAMPLE` interval, the largest delay
in it**. Easy to miss and behaviourally significant for two reasons.

It decorrelates the observations. A hundred frames stuck behind one queue are
one event, not a hundred, and weighting them as a hundred would let frame rate
set the time constant.

It turns the forget factor into a wall-clock time constant, which is the only
form worth stating: the histogram halves a sample's weight after
`RESAMPLE * ln(0.5) / ln(FORGET)`, about **20 s**, and retains 1% of it after
about **135 s**. The forget factor decays **once per resampled observation, not
once per arrival**. Applying a per-observation factor per arrival is the mistake
the #3517 branch makes, and the resulting number reads plausible either way.

### The histogram

An empirical distribution of `BUCKETS` buckets `BUCKET` wide, holding total
weight 1. Each observation is an exponential update:

```
histogram = forget * histogram + (1 - forget) * x
```

where `x` is all weight on the observed bucket. Weight stays normalised for
free, so there is no separate renormalisation step and no running total.

A delay at or beyond `BUCKETS * BUCKET` lands in the last bucket rather than
being discarded. WebRTC discards it, which makes a genuine multi-second spread
indistinguishable from none; the target is bounded by its own ceiling instead.

The cold start is seeded with a decaying prior, `PRIOR[i] = 0.5^(i+1)`. Its
95th percentile falls in bucket 4, so a receiver that has observed nothing at
all targets **100 ms plus one frame**. That number is a consequence of the
prior, not a chosen default.

`RAMP` holds the forget factor below `FORGET` for the first observations, so
early evidence overwrites the prior instead of being averaged into it. It
reaches `FORGET` after about 116 observations, roughly a minute, and has no
effect after that. Without it a receiver crawls toward the truth for a minute;
with it, the corpus's evenly paced trace converges in under two seconds.

### Reading the target

`measured` is the **upper edge** of the first bucket whose cumulative weight
exceeds `QUANTILE`. The upper edge, because the delays in that bucket sit
somewhere inside it and covering the whole bucket is what actually covers the
quantile.

A quantile of an empirical distribution, rather than a mean plus k deviations,
because network delay is heavy-tailed and one-sided. A Gaussian assumption
sizes that tail wrong in both directions.

## The frame duration

One frame duration is added on top of `measured`. The buffer swings by the
spread the quantile measures, so without it the bottom of the swing lands on an
empty ring.

**Frame duration is a codec property. It is never learned from observed
timestamps.** Deriving it from timestamp arithmetic makes a tune-in artifact
indistinguishable from a real frame duration, which is precisely how the #3517
branch reaches a 14.56 s target. A receiver takes the first of these that
applies:

1. The container's own per-sample duration, when it carries one. CMAF does.
2. The codec's in-band duration. An Opus packet states its own duration in its
   TOC byte (RFC 6716 §3.1), at 48 kHz regardless of the encoder's internal
   bandwidth, which covers every duration Opus can pick from 2.5 to 60 ms. This
   is available at the observation point, needs no catalog change, and
   `rs/moq-mux` already has the parser.
3. A codec constant over the catalog's sample rate: 1024 samples for AAC-LC,
   2048 for AAC-HE, 1152 or 576 for MP3 depending on the MPEG version.
4. Nothing. The term is zero and the target is the measured part alone.

A new catalog field was considered and rejected. `AudioConfig` carries
`sampleRate` and `bitrate` and concedes in its own comment that frame duration
"depends on the codec, sample rate, etc"; adding one would mean the full
cross-package sync, in `rs/hang`, `js/hang`, this page, and the hang draft, to
carry a number the payload already states.

### What is not in the frame duration

The browser's AudioWorklet renders in fixed 128-sample blocks, and
`js/watch/src/audio/config.ts` currently adds that block to the per-codec frame
duration, so 48 kHz Opus reads as 23 ms rather than 20 ms. **That block is not
part of the target.** It is a property of the render backend, not of the
network, and native has no worklet at all. If one language's step carried it and
the other's did not, the two would produce different target series from the same
trace and the corpus could not hold.

It belongs in the **render slack**: the margin the playback ring keeps above the
target so a render callback never arrives to find exactly zero samples. In the
browser that is the worklet quantum; natively it is the backend's device period.
Each side sizes its own, above the shared target, and neither is this page's
business.

## The advertised flush span, and how it composes

A hang rendition may advertise `jitter`: the largest delay the publisher has
seen between a frame being ready and handing it to the transport. It is
described in [hang](/concept/hang) and specified in
[draft-lcurley-moq-hang](/draft/moq-hang). The two quantities share a word and
get confused for each other, so plainly:

| | Advertised `jitter` | The estimate here |
| --- | --- | --- |
| Measured by | the publisher, once | each receiver, continuously |
| Measured at | the transport handoff | the receiver's transport |
| Covers | encoder latency, packet packing, reorder, segmentation | all of that, plus the network |
| Is | a declared lower bound | an observation |

**They compose as a maximum, and the reason matters.** Both are the same
formula, lateness relative to a windowed minimum, evaluated one hop apart. The
receiver measures from the frame's media timestamp to its own arrival, so the
publisher's flush delay is already inside every number on this page: a publisher
flushing 100 ms of media at once produces arrivals whose relative delay reaches
100 ms less one frame, on a perfect network, with no help from the catalog.

So the advertised value is a **floor**, not an addend:

```
target = max(measured, advertised) + frame
```

Adding them double-counts the publisher, and doubles the buffer for exactly the
publishers that need it most. Taking the maximum on the grounds that they
"describe the same quantity" reaches the right operation by the wrong argument:
they are the same quantity measured at different points, and the receiver's
point strictly contains the publisher's. The floor earns its place by being
correct *immediately*, before the estimator has seen a full flush cycle, which
is the one thing a measurement cannot be.

Note that `js/watch` today adds the two rather than taking the maximum, which
over-buffers a bursty publisher by its own flush span.

## Rise, fall, startup, and the ceiling

There is **no separate rise or fall limiter**. The histogram is the only
dynamic, and a second rate limit on top of it would be two knobs describing one
behaviour.

The asymmetry a playout target needs falls out of the shape rather than being
dialled in. An underrun is an audible glitch and excess buffer is latency nobody
can hear, so the target should rise readily and fall grudgingly, and it does:

- **Rising** is driven by the maximum-over-interval resampling. A single late
  arrival defines its whole interval, so a burst is weighted as heavily as the
  hundreds of punctual frames around it.
- **Falling** is driven by the forget factor alone, over the 20 s half-life
  above. Old mass in the tail has to decay out before the quantile can move
  down, and nothing can make it decay faster.
- **A single bad observation does not move the target at all.** It adds at most
  `1 - forget` weight, which past the startup ramp is 1.7%, well under the 5%
  the 95th percentile needs. It takes a few intervals of genuine lateness before
  the target responds, which is the correct answer to a one-off straggler.

The #3517 branch needed an explicit rise clamp and a one-frame-per-second fall
limiter because it had an unbounded instant-rise path that bypassed the
histogram entirely: `percentile() + step` was published the moment it rose, and
`step` was learned from observed timestamps. Removing that path removes the need
for the limiters.

**The ceiling is structural.** `measured` cannot exceed `BUCKETS * BUCKET`, so
nothing the receiver observes can carry the target past `BUCKETS * BUCKET +
frame`, about 2 s. There is no magic number to clamp against, which is the
specific failure of the #3517 branch: it clamped the percentile and then added
an unbounded frame term on top.

The advertised floor is the one term that can exceed that bound, and it is
deliberately not clamped to it. The ceiling is a property of the histogram's
size; the floor is a number the publisher declared about itself, so clamping
the floor to the ceiling would under-buffer precisely the publisher that was
honest about a long flush.

Policy limits sit above the algorithm. A receiver's own configured maximum and
the track's retention window both cap what it may hold, and neither is the
estimator's business.

## Idle intervals

An interval with no arrival in it still folds, seeded with the **prior** rather
than with an observed bucket:

```
histogram = forget * histogram + (1 - forget) * PRIOR
```

This is the only honest answer to "does the wall-clock memory survive a pause".
Skipping empty intervals freezes a stale estimate indefinitely. Decaying without
seeding un-normalises the histogram and the quantile falls through to the
ceiling. Seeding with the prior relaxes the estimate back toward cold start at
the same time constant as everything else, which is what an evidence-free
interval should do, and it raises the target rather than lowering it, so the
error is on the safe side.

`IDLE_MAX` caps how many intervals are folded at once. It exists only to bound
the work: after 1024 folds the retained weight is about 2e-8, so the cap changes
no output an `f64` can represent.

The startup ramp is not restarted. A receiver coming back from a pause is not
cold, and re-converging at the base rate is correct.

## Fixed targets

An explicit delay **fixes the target and disables adaptation**. It is taken
literally: no advertised floor, no frame term, no estimate.

```
target = delay
```

An impossible fixed target is refused, not silently adjusted. A negative delay,
or one beyond the track's retention window, is an error at configuration time.
Supported or refused, not warned about and continued.

In the browser this is the numeric `delay` setting; natively it is
`decode::Options::delay`, where `None` selects the estimator. The two spell the
same thing the same way on purpose. It is distinct from `max_age`, which is the
live-edge skip budget and not a buffer.

## Constants

Every value comes from WebRTC's NetEq, whose algorithm lives in
`underrun_optimizer`, `histogram`, and `packet_arrival_history` under
`modules/audio_coding/neteq/`, not in the `delay_manager.cc` the name suggests.
The two exceptions are marked.

| Constant | Value | Origin |
| --- | --- | --- |
| `WINDOW` | 2000 ms | `PacketArrivalHistory` window size |
| `BUCKET` | 20 ms | `Histogram` bucket size |
| `BUCKETS` | 100 | `UnderrunOptimizer` bucket count |
| `QUANTILE` | 0.95 | NetEq's default delay histogram quantile |
| `FORGET` | 0.983 | NetEq's underrun forget factor. The widely repeated 0.97 is a transitional value, and 0.9993 is the *reorder* factor, which the #3517 branch uses by mistake |
| `RESAMPLE` | 500 ms | NetEq's resample interval |
| `RAMP` | 2 | NetEq's start forget weight |
| `PRIOR[i]` | `0.5^(i+1)` | NetEq's histogram seed |
| `IDLE_MAX` | 1024 intervals | Ours. A work bound with no effect on output |
| Out-of-range delays | clamped, not dropped | Ours. See [the histogram](#the-histogram) |

## Implementing from this page

WebRTC is BSD-3 plus a patents grant, and copying its source would carry a third
license and its disclaimer into an `MIT OR Apache-2.0` repository. Implement
from the description above, citing NetEq as the design's source in a comment.
SpeexDSP's `jitter.c` is BSD-3 and safe to read. Asterisk's `jitterbuf.c`
(GPLv2) and FreeSWITCH's `switch_jitterbuffer.c` (MPL 1.1) are not.

No crate was adopted. The `neteq` crate is the only viable candidate and takes a
percentile over per-packet deltas each clamped at zero, which cannot see a
build-up, the one property this design exists for. It is also a whole NetEq,
sample-rate-locked ring buffers and WSOLA and packet loss concealment, where
this is a few hundred lines of estimator whose shape would then be its. Neither
`webrtc-rs` nor `str0m` implements playout at all, GStreamer's
`rtpjitterbuffer` solves clock skew rather than target sizing and is LGPL, and
no browser-usable port exists.

## Conformance corpus

`doc/concept/audio-jitter/` holds one JSON file per case: an arrival trace and
the target series it must produce. Both implementations read these files
directly, so neither is graded against the other.

```json
{
  "name": "tunein",
  "about": "One frame from a stale group, then the live edge 14.5 s later, ...",
  "frame": 20,
  "arrival": [0, 10, 30],
  "media": [0, 14500, 14520],
  "target": [[0, 120], [26, 80]]
}
```

`arrival` and `media` are parallel arrays in **delivery order**, the order the
receiver observes them, so `media` is deliberately out of order in the
reordering case. `advertised` and `delay` are present only where the case uses
them. `target` lists `[frame index, target]` at every frame where the target
differs from the previous frame's, starting at frame 0, so an implementation
folds the trace one frame at a time and compares its target at each listed
index. Tolerance is 1e-6 ms, which the `f64` rules above make generous.

| Case | Holds the implementation to |
| --- | --- |
| `paced` | An even sender converges to one bucket plus one frame, and the startup ramp gets it there in under two seconds |
| `flush` | The measured term follows a flush span, and tracks it when the span doubles |
| `advertised` | The advertised span floors the target instead of adding to it |
| `buildup` | A queue filling at 2 ms per frame is measured against the fastest recent arrival |
| `reorder` | Out-of-order media timestamps never reach the delay measurement |
| `idle` | A 30 s gap relaxes the histogram toward the prior |
| `tunein` | A stale frame then the live edge 14.5 s later leaves the target near the frame duration. This is the #3517 regression |
| `spike` | A recurring stall raises the target, and it decays once the stalls stop |
| `fixed` | An explicit delay ignores the arrivals entirely |

`reference.ts` beside them generated the numbers. It is **not normative and not
shipped**: this page is. When the two disagree, this page wins, the reference is
corrected, and the corpus is regenerated with
`bun doc/concept/audio-jitter/corpus.ts`. An implementation that imports the
reference has proved nothing, so none of them may; `biome.jsonc` forbids the
import outside this directory rather than leaving it to review.

`corpus.test.ts` re-runs the reference and fails when the checked-in files no
longer match it, which is what stops a file being edited by hand to make a
failing implementation pass. It compares rather than rewrites; regenerating is
the `bun doc/concept/audio-jitter/corpus.ts` command above. It runs in
`just test` and `just check`.
