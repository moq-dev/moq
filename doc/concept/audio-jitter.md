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
age budget.

Natively that holds only as far as the caller lets it. The container consumer
reads groups in sequence order and applies its age budget inside `read()`, so a
group skipped for age is never observed, and a frame queued behind a stalled
group is observed when the stall clears. A caller that estimates keeps the budget
at least as long as the estimate's ceiling, which is why `moq play` waits 2 s on
a stalled group under `--delay auto`. The observation is also only as honest as
the caller is prompt: `read()` is timed when it resolves, so a player drains it
as packets come and lets the playback buffer hold the target, rather than
pacing reads to the speaker.

A frame dropped by the relay is never observed, which is correct only while the
drop is not the receiver's own doing. The subscription's max delay drops groups
before any container sees them, at the relay and in the receiver's own transport
subscriber, so a subscription cut to the target hides every frame later than
the target: the estimate creeps up one bucket at a time while the frames it
should have measured go unplayed. In automatic mode both implementations
subscribe with at least the ceiling, 2 s: `moq play --delay auto` through its
age budget, and the browser through the audio subscription alone, since its
container consumer observes each frame before applying the local budget.
Browser video keeps the shared budget, because a longer one fetches whole stale
groups, so a broadcast with no audio measures only the lateness that budget
lets through.
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
    if media > newest:                  # a reordered frame never joins the history
        newest = media
        drop history entries with entry.media < media - WINDOW
        if MAX_HISTORY entries are held: drop the oldest
        push (arrival, media) onto history

    ref = entry in history with entry.media <= media, minimising (entry.arrival - entry.media),
          the oldest on a tie
    if there is none: return            # older than the whole window, not measured
    delay = max(0, (arrival - ref.arrival) - (media - ref.media))

    if first observation:
        origin = arrival; interval = 0; peak = delay; return

    index = floor((arrival - origin) / RESAMPLE)
    if index <= interval:
        peak = max(peak, delay); return

    fold(bucket of peak)
    fold(nothing) for each elapsed empty interval, at most IDLE_MAX
    interval = index; peak = delay

fold(bucket):                           # prior starts at 1
    rest = 1 - FORGET * (1 - prior)     # what the decayed evidence leaves
    fresh = min(FRESH_MAX, rest) if bucket else 0
    histogram = FORGET * histogram + (rest - fresh - FORGET * prior) * PRIOR
    histogram[bucket] += fresh
    prior = rest - fresh

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

### Bounding the history

The history holds at most `MAX_HISTORY` (8192) arrivals. The window is media
time and the publisher chooses the timestamp rate, so without a cap a stream
that advances a nanosecond per frame keeps every frame it sends. The cap stays
far above the two-second window of any real track, including a 1000 fps one
(2000 entries); a stream dense enough to reach it measures against a window
shorter than `WINDOW` instead of growing without bound.

The reference is a minimum over a prefix of the history, and the publisher
chooses the prefix by reordering, so scanning for it lets every frame cost
`MAX_HISTORY` comparisons. Keep the history in a structure that answers any
prefix's minimum in logarithmic time, such as a segment tree over a ring.
Keep the oldest entry on a tie: tied entries with a different `arrival` and
`media` round `delay` differently.

### Reordered arrivals are measured against what came before them

A frame whose media timestamp is not newer than the newest already observed is
**reordered**. It is measured, against the fastest frame **at or before it in
media time**, and it never joins the history.

It is measured because the player waits for it. The container consumer takes
groups off the transport in delivery order and plays them in media order, and a
relay sends a track's newest group first. A publisher that flushes several
one-frame groups at once, over a path that cannot carry them together, has the
newest frame of each flush arrive first and the oldest, the one played next,
arrive last. Excluding the reordered frames measures only the punctual head of
every flush, so the target settles below the wait the playhead actually makes,
and the receiver runs dry and refills on every flush. That is the re-stall loop
the nightly harness measured on AAC under bursty delivery, where most frames
arrived reordered.

It is measured only against earlier frames because of what a subscription's
start delivers: its live edge first, then the backlog behind it, newest first.
Measured against the live edge, a backlog frame's whole media-time distance would
be *added* to its delay, which is exactly the failure this design exists to
avoid. The backlog is older than every frame in the history, so it has no
reference and is not measured, the same as a frame the window already pruned.
An in-order frame gets the same rule for free, since every entry before it is
older.

It never joins the history because it can never be a reference worth having: it
arrived after a newer frame, so it is never the fastest. Keeping it out is also
what leaves the next frame of a backlog without a reference.

Pricing reordering is still out of scope. A receiver that would rather skip a
late frame than wait for it weighs late arrivals against the loss of playing
without them, which is WebRTC's `ReorderOptimizer` and SpeexDSP's
`compute_opt_delay`. This estimator sizes the wait for every frame it measures.

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
weight 1. In steady state each observation is an exponential update:

```
histogram = FORGET * histogram + (1 - FORGET) * x
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

The histogram holds that prior at weight `prior`, starting at 1, plus
evidence. Evidence always
decays by `FORGET`, and whatever it leaves is the prior's, of which an
observation takes up to `FRESH_MAX`. So early evidence overwrites the prior
instead of being averaged into it: each of the first observations takes a
tenth of the weight, the prior is spent within about a dozen of them, and the
corpus's evenly paced trace converges in five seconds. From then on every
observation takes `1 - FORGET`, which is the update above.

The cap keeps one interval from raising the target only to drop it again.
NetEq instead ramps the forget factor itself, `count / (count + 2)`, which
reaches `FORGET` after about a minute and until then decays old evidence as
fast as the prior. On a bursty AAC path that let one late flush 6 s in carry
14% of the histogram, raising the target by about 300 ms, and forgot it within
5 s: playback parked to refill on the rise, skipped as much buffered audio on
the fall, and stalled again on the next late flush. Under the cap a lone
observation holds at most `FRESH_MAX`, twice the 95th percentile's tail, so it
moves the target only while the prior has weight left, and for at most one
20 s half-life.

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

The browser's AudioWorklet renders in fixed 128-sample blocks. **That block is
not part of the target.** It is a property of the render backend, not of the
network, and native has no worklet at all. If one language's step carried it and
the other's did not, the two would produce different target series from the same
trace and the corpus could not hold.

It belongs in the **render slack**: the margin the playback ring keeps above the
target so a render callback never arrives to find exactly zero samples. In the
browser that is the worklet quantum; natively it is the backend's device period.
Each side sizes its own, above the shared target, and neither is this page's
business.

## The advertised flush span, and how it composes

A hang rendition may advertise `jitter`: the largest span of media the
publisher writes behind a frame's timestamp, from B-frame reordering or a batch
written together. It is
described in [hang](/concept/hang) and specified in
[draft-lcurley-moq-hang](/draft/moq-hang). The two quantities share a word and
get confused for each other, so plainly:

| | Advertised `jitter` | The estimate here |
| --- | --- | --- |
| Measured by | the publisher, once | each receiver, continuously |
| Measured from | media timestamps | the receiver's arrival times |
| Covers | packet packing, reorder, segmentation | all of that, plus encoder latency and the network |
| Is | a declared lower bound | an observation |

**They compose as a maximum, and the reason matters.** Both are lateness
behind the media timestamp, evaluated one hop apart. The receiver measures from
the frame's media timestamp to its own arrival, so the publisher's batching is
already inside every number on this page: a publisher writing 100 ms of media at
once produces arrivals whose relative delay reaches
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
correct *immediately*, before the estimator has seen a full batch, which
is the one thing a measurement cannot be.

## Across renditions

Every track plays from one clock, anchored on the earliest lateness any track
has shown. The estimator measures each track against its own fastest frame, so a
constant offset between tracks cancels out of `measured`: a video encoder that
flushes 200 ms behind the audio encoder produces the same video target as one
that does not, and its frames would all arrive 200 ms late for the shared clock.

The catalog `delay` field carries that offset: how far a rendition's
timestamps trail those the broadcast's other renditions already wrote, measured
by the publisher and never lowered. It is an addend, not a floor, because nothing on
this page contains it. The shared playout is the largest requirement among the
renditions actually subscribed:

```
playout = max over subscribed renditions of (delay + target)
```

A receiver **must not** subtract one rendition's `delay` from another's. Each is
a lifetime maximum taken against whichever rendition was ahead, so two values need not
share an origin: if the earliest rendition later drifts behind another, their
difference understates the real spread. The maximum never under-buffers,
because the subscribed renditions' spread is measured from an earliest baseline
no earlier than the broadcast's, so it never exceeds the largest `delay` among
them. The cost is over-buffering by the smallest subscribed `delay` when the
broadcast's earliest rendition is not subscribed.

The playout is recomputed when a rendition is subscribed, unsubscribed, or its
catalog entry rises, so dropping a slow rendition lowers latency. The shared
reference itself only ever moves earlier, so once the earliest subscribed track
leaves, the clock stays anchored to it until playback re-anchors; that errs on
the safe side.

A track that replaces another in the same slot and on the same broadcast path,
such as the new name a codec change publishes under, continues the outgoing
track's histogram, prior weight included, instead of starting over. The path's
arrivals did not change with the name; a track on another path starts from the prior.
Restarting would drop the target to the cold-start 100 ms at the switch, then
raise it as the lateness is relearned, and the playout follows both moves:
buffered audio is skipped on the drop and playback parks on each rise. Only the
reference window restarts, so the new track's frames are measured against its
own fastest. Its opening backlog would read as seconds late against the old
track's arrivals.

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
- **A single bad observation does not move the target on its own.** Once the
  prior is spent it adds `1 - FORGET` weight, 1.7%, well under the 5% the 95th
  percentile needs. It takes a few intervals of genuine lateness before the
  target responds, which is the correct answer to a one-off straggler. A path
  whose recent lateness already holds nearly 5% of the weight sits on the
  boundary, so one more late interval tips it up for a fold or two. Only in
  the first few seconds, while an observation can take up to `FRESH_MAX`, can
  one interval move it, and then it decays like any other evidence.

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

A receiver coming back from a pause is not cold: the evidence that survived
keeps decaying at the base rate. Only the weight the prior regained is replaced
quickly, the same way it is at cold start, since it stands for no evidence at
all.

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
same thing the same way on purpose. It is distinct from `max_delay`, which is the
live-edge skip budget and not a buffer.

## Constants

Every value comes from WebRTC's NetEq, whose algorithm lives in
`underrun_optimizer`, `histogram`, and `packet_arrival_history` under
`modules/audio_coding/neteq/`, not in the `delay_manager.cc` the name suggests.
The exceptions are marked.

| Constant | Value | Origin |
| --- | --- | --- |
| `WINDOW` | 2000 ms | `PacketArrivalHistory` window size |
| `BUCKET` | 20 ms | `Histogram` bucket size |
| `BUCKETS` | 100 | `UnderrunOptimizer` bucket count |
| `QUANTILE` | 0.95 | NetEq's default delay histogram quantile |
| `FORGET` | 0.983 | NetEq's underrun forget factor. The widely repeated 0.97 is a transitional value, and 0.9993 is the *reorder* factor, which the #3517 branch uses by mistake |
| `RESAMPLE` | 500 ms | NetEq's resample interval |
| `FRESH_MAX` | 0.1 | Ours, twice the quantile's tail. NetEq ramps the forget factor from a start weight of 2 instead; see [the histogram](#the-histogram) |
| `PRIOR[i]` | `0.5^(i+1)` | NetEq's histogram seed |
| `IDLE_MAX` | 1024 intervals | Ours. A work bound with no effect on output |
| `MAX_HISTORY` | 8192 arrivals | Ours. Bounds a rate the publisher chooses; see [Bounding the history](#bounding-the-history) |
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
`reorder` and `backlog` cases. `advertised` and `delay` are present only where the case uses
them. `target` lists `[frame index, target]` at every frame where the target
differs from the previous frame's, starting at frame 0, so an implementation
folds the trace one frame at a time and compares its target at each listed
index. Tolerance is 1e-6 ms, which the `f64` rules above make generous.

| Case | Holds the implementation to |
| --- | --- |
| `paced` | An even sender converges to one bucket plus one frame, and replacing the prior gets it there in five seconds |
| `flush` | The measured term follows a flush span, and tracks it when the span doubles |
| `advertised` | The advertised span floors the target instead of adding to it |
| `buildup` | A queue filling at 2 ms per frame is measured against the fastest recent arrival |
| `reorder` | A run delivered after a newer one is measured against the frames before it, so the target covers waiting for it |
| `backlog` | A backlog delivered newest first behind the live edge is not measured, so its media span never reaches the target |
| `idle` | A 30 s gap relaxes the histogram toward the prior |
| `tunein` | A stale frame then the live edge 14.5 s later leaves the target near the frame duration. This is the #3517 regression |
| `spike` | A recurring stall raises the target, and it decays once the stalls stop |
| `straggler` | One late interval after the prior is spent leaves the target where it was |
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
`just check`.
