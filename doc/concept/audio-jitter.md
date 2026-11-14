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
group is observed when the stall clears. A missing group, one that has delivered
no frame, is waited for only the playout delay plus any lookahead, since past that
it would play late: a reordered backlog's holes waited out to the whole budget
would delay the frames behind them, which the estimator would measure as
jitter. A caller that estimates keeps the budget past the target, which is why
`moq play` waits on a stalled audio group for the delay plus the estimate's 2 s
ceiling. The observation is also only as honest as
the caller is prompt: `read()` is timed when it resolves, so a player drains it
as packets come and lets the playback buffer hold the target, rather than
pacing reads to the speaker.

A frame dropped by the relay is never observed, which is correct only while the
drop is not the receiver's own doing. The subscription's max delay drops groups
before any container sees them, at the relay and in the receiver's own transport
subscriber, so a subscription cut to the target hides every frame later than
the target: the estimate creeps up one bucket at a time while the frames it
should have measured go unplayed. Both implementations subscribe with the delay
plus the estimate's own 2 s ceiling, whatever the player's `stretch` setting:
`moq play` through its age budget, and the browser through the audio
subscription alone, since its container consumer observes each frame before
applying the local budget. So any lateness the estimator can measure is
observed at once. A buffered browser player keeps its subscription at least the
delay plus its lookahead.
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

    fastest = min over history of (entry.arrival - entry.media)
    if first observation: start = arrival; joined = arrival
    elif media + fastest < start:       # produced before the subscription: backlog
        joined = max(joined, arrival)

    ref = entry in history with entry.media <= media, minimising (entry.arrival - entry.media),
          the oldest on a tie
    if there is none: return            # older than the whole window, not measured
    delay = max(0, (arrival - ref.arrival) - (media - ref.media))

    if first observation:
        origin = arrival; interval = 0; peak = delay; oldest = media
        note(arrival, delay); return

    index = floor((arrival - origin) / RESAMPLE)
    resumed = ended; ended = false
    if index <= interval:
        peak = max(peak, delay); oldest = min(oldest, media)
        note(arrival, delay); return

    if oldest + fastest < joined:       # the join, not the path: folds nothing
        pending += 1
    else:
        close(bucket of peak)
    if resumed:                         # a declared silence folds nothing
        settle(); origin = arrival; index = 0
    else:                               # held until this interval closes
        pending += index - interval - 1
    interval = index; peak = delay; oldest = media; due = +inf; late = -inf
    note(arrival, delay)

note(arrival, delay):                   # due and late start at +inf and -inf
    if delay > RESAMPLE:                # stalled: due in an earlier interval
        due = min(due, arrival - delay); late = max(late, arrival)

close(bucket):                          # pending starts at 0 and is spent here
    if late > -inf and a stall is open
            and due < stall.late and due - stall.due < WINDOW:
        stall.bucket = max(stall.bucket, bucket); stall.late = late
        stall.span += pending + 1; pending = 0
        return                          # still behind the same stall
    settle()
    if late > -inf: open a stall with bucket, late, due, span = pending
    else: fold(nothing) min(pending, IDLE_MAX) times; fold(bucket)
    pending = 0

settle():                               # a stall is one observation
    if a stall is open:
        fold(stall.bucket); fold(nothing) min(stall.span, IDLE_MAX) times; close it

end():                                  # a declared end precedes the next observation
    if not first observation: ended = true

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

It is measured only against earlier frames because a newer reference would add
the media-time distance between them to the frame's delay, which is exactly the
failure this design exists to avoid. A frame older than every frame in the
history has no reference and is not measured, the same as a frame the window
already pruned. An in-order frame gets the same rule for free, since every entry
before it is older. A subscription's backlog is the [join's](#the-joins-backlog)
to discount.

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

### One observation per stall

An outage spans intervals, and the max per interval alone would count it once
per interval. A link that stops for 1.5 s and then delivers its backlog would
fold four or five peaks of a second or more, far past the 5% the quantile
needs, and a one-off would hold the target, and video with it, a second behind
live for 10 to 25 s after the link recovered. Those intervals are one event for
the same reason a queue's hundred frames are.

A frame whose delay exceeds `RESAMPLE` was due in an earlier interval than the
one it arrived in, so it is **stalled**, and `arrival - delay` is when it was
due. An interval holding a stalled frame due before the open stall's latest
stalled frame arrived is still behind that stall: it joins it instead of
folding. The empty intervals before a stalled interval belong to its stall as
well, since a link that stops outright delivers nothing until its backlog
arrives, so empty intervals fold only once the interval after them closes and
shows whether it is stalled: a recovery that delivers the live edge before its
backlog is still a stall. The stall ends at the
first interval that does not continue it, and only then folds, once, its
largest delay, then one idle fold for each other interval it spanned. So the
estimate keeps its wall-clock time constant, and the stall's single observation
does not take the weight the idle folds return to the prior, which an
observation after them would reclaim up to `FRESH_MAX`.

The test is on due time, not on adjacency, so a publisher that flushes a second
or more of media at once still counts each flush: the first frame of the next
flush was due only after the previous one arrived. Flushes back to back and
newest first do chain, since each is due before the previous one finished
arriving, and locally they look like outages one after another. So a stall
covers at most `WINDOW` of due time, the span the delay itself is measured
over, and longer lateness folds once per window as the recurring stall it is.
Recurring outages still
raise the target, a few of them within a half-life, which is the evidence a
recurring stall deserves. A lateness under one interval never stalls, so
ordinary jitter, shorter flushes, reordering, and a step in the path's delay fold one
observation per interval.

The open stall is four numbers, so a publisher that chains stalls on purpose
costs nothing. While due times advance, one fold per window of due time bounds
how long the estimate goes without evidence. Only a publisher repeating an old
timestamp holds them still, since a frame more than `WINDOW` behind the newest
is not measured, and that holds its own stall open: it freezes its own
broadcast's estimate, at no cost beyond the four numbers.

### The join's backlog

A subscription opens with the backlog its relay holds, delivered faster than real
time and interleaved with the live edge, and the live frames produced meanwhile
queue behind it. Measured, the backlog frames read late against the older backlog
before them and the queued live frames late against the edge: 250 to 760 ms on
the nightly and recorded joins. The cold start would fold that at `FRESH_MAX` and
hold the target, and video with it, there for a 20 s half-life after every
join. The player never waits for any of it, since a join skips onto the clock.

`fastest`, the window's smallest `arrival - media`, is the fastest path seen, so
`media + fastest` is when a frame was due. A frame due before `start`, the first
arrival, was produced before the subscription began: it is **backlog**. `joined`
is the latest arrival of a backlog frame, `start` until one arrives. An interval
holding a frame due before `joined` measured the join, not the path: its frames
are backlog or queued behind it, so it folds nothing, as if it held no arrival.

The test runs when the interval closes, not per frame, because a join reveals
itself late: the fastest path moves earlier as the backlog drains, 1.3 to 2 s
over the first 400 to 800 ms of the nightly joins, so a frame punctual against
the backlog before it can turn out to be due before `start`. Only backlog moves
`joined`, so lateness after the join never extends it: an outage's frames were
produced after the subscription began, and a publisher flushing seconds at once
is measured from its first flush past the backlog. A first frame slower than the
path after it is itself backlog, so most subscriptions skip their first interval,
which leaves the prior 500 ms later.

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

The catalog `delay` field carries that offset. It is an addend, not a floor,
because nothing on this page contains it, and it is never lowered.

A publisher that knows the offset states it, and it is used as given. The
receiver already adds a frame duration on top, so an encoder configured not to
reorder or queue frames, whose frames trail by no more than their own duration,
states zero. moq-video does so for a backend that confirmed its preset, and
moq-audio for Opus and PCM captured at the codec's rate, since a resampler
holds input. Anything else is measured: the least a rendition's
timestamps trailed those the broadcast's other renditions already wrote, over a
window of a second or a group of its media, whichever is longer, keeping the
largest of those windows. A measured value is reported only once a window
closes.

Lateness only some frames have is left out, since `measured` already contains
it. A B-frame written after a later reference picture trails that picture and
another rendition's, and the audio written beside it trails them too. The
reference pictures do not, and every window holds one.

The shared playout is the largest requirement among the renditions actually
subscribed:

```
playout = max over subscribed renditions of (delay + target)
```

A receiver **must not** subtract one rendition's `delay` from another's. Each is
taken against whichever rendition was ahead and only ever rises, so two values
need not share an origin: if the earliest rendition later drifts behind another,
their difference understates the real spread. The maximum never under-buffers,
because the subscribed renditions' spread is measured from an earliest baseline
no earlier than the broadcast's, so it never exceeds the largest `delay` among
them. The cost is over-buffering by the smallest subscribed `delay` when the
broadcast's earliest rendition is not subscribed.

The playout is recomputed when a rendition is subscribed, unsubscribed, or its
catalog entry rises, so dropping a slow rendition lowers latency. Each track's
own arrivals only ever move its part of the shared reference earlier. A track
that leaves, or whose timeline restarts, releases its part: the earliest
remaining track's anchor takes over, or the next frame re-anchors.

A track that replaces another in the same slot and on the same broadcast path,
such as the new name a codec change publishes under, continues the outgoing
track's histogram, prior weight included, instead of starting over. The path's
arrivals did not change with the name; a track on another path starts from the prior.
Restarting would drop the target to the cold-start 100 ms at the switch, then
raise it as the lateness is relearned, and the playout follows both moves. Only the
reference window restarts, so the new track's frames are measured against its
own fastest. Its opening backlog would read as seconds late against the old
track's arrivals. What the outgoing track measured but has not folded, its
interval in progress and any open stall, folds into the continued histogram as
if that interval closed at the switch, judged by the outgoing track's join: a switch most often follows an outage,
and would otherwise drop the outage's one observation.

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
  down, and nothing can make it decay faster. What keeps a one-off from holding
  the target is that it puts little mass there: an outage is
  [one observation](#one-observation-per-stall), however many intervals its
  backlog spans, while its stalled frames fall within `WINDOW` of due time;
  a longer one is an observation per window.
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
honest about a long flush. Honoring it must not cost memory in proportion to
the declared number, so the browser decodes at most 500 ms of any target and
holds the rest as the encoded frames it actually received.

Policy limits sit above the algorithm. A receiver's own configured maximum and
the track's max age both cap what it may hold, and neither is the
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

Idle intervals fold when the interval after them closes, before it, unless
that interval is [stalled](#one-observation-per-stall), whose stall owns them.

`IDLE_MAX` caps how many intervals are folded at once. It exists only to bound
the work: after 1024 folds the retained weight is about 2e-8, so the cap changes
no output an `f64` can represent.

A receiver coming back from a pause is not cold: the evidence that survived
keeps decaying at the base rate. Only the weight the prior regained is replaced
quickly, the same way it is at cold start, since it stands for no evidence at
all.

### Declared silence

A publisher that stops sending on purpose, such as a muted push-to-talk
speaker, marks the stop with a declared end. The silence after it says
nothing about the path, so relaxing toward the prior would only make a
listener who sat through a long mute come back over-buffered. `end()` tells
the estimator that a declared end lies between the previous observation and
the next. The next measured observation then folds the pending interval as
usual and starts a fresh interval at its own arrival instead of folding the
intervals in between. If it lands in the pending interval it is merged as
usual, since there is nothing to skip. Either way the end is spent, so a
later gap the publisher did not declare still relaxes the estimate. The
reference history, the histogram, and the prior are untouched, and an end
before the first observation, or under a fixed target, does nothing.

The receiver calls `end()` before observing the first frame from a group later
than the declared end's own: the stopped encoder's final packets may follow the
marker in its group, and they belong to the speech before the silence, not after it.

## Fixed targets

An explicit delay **fixes the target and disables adaptation**. It is taken
literally: no advertised floor, no frame term, no estimate.

```
target = delay
```

An impossible fixed target is refused, not silently adjusted. A negative delay,
or one beyond the track's max age, is an error at configuration time.
Supported or refused, not warned about and continued.

In the browser this is the numeric `delay` setting; natively it is
`decode::Options::delay.min` with `decode::Options::estimate` set to `false`.
`estimate` defaults to `true`, which runs the estimator instead and never goes
below that `min`. The two spell the same thing the same way on purpose. It is
distinct from `delay.max`, which is the live-edge skip budget and not a buffer.
The same `min` is where a cold join starts.

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
them, as is `ends`: the indices, ascending, of the frames observed right after a
[declared end](#declared-silence), each of which calls `end()` before it is observed. `target` lists `[frame index, target]` at every frame where the target
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
| `join` | A recorded join whose backlog arrives interleaved with the live edge, and delays the live frames queued behind it, folds nothing, so the target leaves the prior for the path's own |
| `idle` | A 30 s gap relaxes the histogram toward the prior |
| `silence` | A declared 60 s silence and a declared pause leave the target where it was, and a later undeclared gap still relaxes it |
| `tunein` | A stale frame then the live edge 14.5 s later leaves the target near the frame duration. This is the #3517 regression |
| `spike` | A recurring stall raises the target, and it decays once the stalls stop |
| `straggler` | One late interval after the prior is spent leaves the target where it was |
| `batches` | Back-to-back 1 s flushes delivered newest first chain their stalls, which still fold once per window of due time and raise the target |
| `outage` | A one-off 1.5 s outage whose backlog arrives over several intervals is one stall and leaves the target where it was, while three more 3 s apart raise it |
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

## Received-audio time stretch

Every player plays received PCM onto the shared clock by one rule driven by
audio's lag behind that clock: native playback in `rs/moq-audio`, and the
browser in the AudioWorklet's ring in `js/watch`, live or buffered. Neither
generates replacement audio for a missing packet, so a hole in the media plays
as a hole. The estimator above is unchanged. Both share the decision, the
splice, and the [corpus](#stretch-corpus) below.

So in steady state audio plays on the shared clock at the playout delay. Audio
off the clock by less than the `stretch` threshold plays faster or slower onto
it, at a rate proportional to its lag and at most 10% off real time, for any
content; past the threshold it snaps onto the clock, as video does. When the
audio received ahead of the playhead is about to run out, playback slows down
to bridge the gap, never trailing the clock past the threshold. A congestion
stall within the threshold costs a slowdown, a gap, and a catch-up; one past it
costs a skip.

The splice is waveform-similarity overlap-add (WSOLA), after Verhelst and
Roelands, "An overlap-add technique based on waveform similarity (WSOLA) for
high quality time-scale modification of speech" (ICASSP 1993), written from
that description rather than from any implementation's source.

### PCM and units

PCM is `f32` in \[-1, 1]; analysis uses `f64`. A frame is one sample across all
channels. All counts below are frames at the output device's rate `R`:

```
hop    = max(1, floor(R * 10 / 1000))   the crossfade, and the spacing of decisions
span   = 2 * hop                        the farthest one splice moves the playhead
window = 3 * hop                        the received audio past the playhead a splice can need
```

`CAP` is 0.1, the most the stretch speeds up or slows down as a fraction of real
time, and `GAIN` is 500 ms: the rate is the lag over it, so a lag past 50 ms
runs at the cap and a smaller one decays with a 500 ms time constant.

### Decision

At each hop boundary a player decides on four inputs, in frames:

- `lag`: how far the playhead trails the shared clock, negative when it plays
  ahead of it. The clock is where it stands at that boundary, so it advances
  through a device read.
- `depth`: the received audio ahead of the playhead, decoded or not.
- `bound`: how far audio may trail the clock: the player's `stretch` setting
  (200 ms by default), or for a buffered player the larger of it and the
  lookahead. It sizes nothing else.
- `read`: what the device reads at once, the device period natively and the
  render quantum in the browser.

With `cushion = 2 * hop + read` and `least = floor(hop / 2)`, the decision is a
`pace` (the rate less real time: positive plays faster), the least and most one
splice may shift the playhead (`low` and `high`; a positive shift skips ahead,
a negative one repeats), and whether it is `urgent`:

1. `bound <= 0`: hold.
2. `depth < cushion` and `lag < bound`: bridge, with `pace = -CAP`,
   `low = ceil(lag - bound)`, `high = -least`, urgent.
3. `|lag| <= least`: hold.
4. Otherwise `pace = clamp(lag * 1000 / (R * GAIN), -CAP, CAP)`, not urgent.
   Behind the clock, `low = least` and
   `high = floor(min(lag + least, depth - cushion))`; ahead of it,
   `low = ceil(lag - least)` and `high = -least`.

Hold is `pace = low = high = 0`, not urgent. So a splice moves the playhead at
least half a hop, never carries it more than half a hop past the clock, never
speeds up into the bridge's cushion, and a bridge never lets audio trail the
clock past the bound. The cushion is the stretch's window less the hop a splice
plays, and one device read: a slowdown must start while a splice still has
received audio to search.

A moved target moves the clock, so a rise leaves audio ahead of it and a fall
leaves audio behind, and both become ordinary slowing down and speeding up
within the bound; past it, a fall skips and a rise pads silence until the clock
arrives, as any lag past it does. A clock whose reference moves earlier jumps
ahead, which is lag like any other.

Without a clock reading the browser holds, so received audio plays unaltered.
Native playback then takes the target as where the playhead belongs, so
`lag` is the received depth less the target, and the bound is three quarters of
its ring's capacity above the target.

### Pace and splice

The stretch holds up to `span` frames of history behind the playhead, and is
topped up to a window past it before each decision while it may stretch. It
keeps three numbers: `owed`, the playhead movement the pace asked for that no
splice took yet; its `direction`; and an `allowance`, which starts at `span`.

At each hop boundary, before anything else, the allowance refills by
`CAP * hop`, up to `span`. If the pace's sign differs from `direction`, it
becomes the direction and `owed` resets to zero. A hold returns no splice.
Otherwise `owed` adds `pace * hop`, clamped to ±`span`, and a splice is tried
once `direction * owed >= hop`, or at once when urgent. The shift's range is
the decision's `[low, high]`, within ±`floor(allowance)`, at least `-head` (the
history held) and at most what is held past the playhead less a hop; when not
urgent, also within a hop of `owed`, `[ceil(owed) - hop, floor(owed) + hop]`.
An empty range, or a zero shift, splices nothing. A splice subtracts its shift
from `owed`, clamped to ±`span`, and its size from the allowance, so past one
span the stretch never moves the playhead faster than the cap.

The search scores every channel's samples together in `f64`, so channels in
opposite phase never cancel and a channel silent in one place never decides
alone. With `x[c][i]` channel `c`'s sample at frame `i` and `p` the playhead, a
shift `s` scores the normalized correlation of the hop at `p` (the natural
continuation) with the hop at `p + s`:

```
score(s) = sum(x[c][p+k] * x[c][p+s+k]) / sqrt(sum(x[c][p+k]^2) * sum(x[c][p+s+k]^2)),
           summed over k in [0, hop) and every channel c
```

or zero when either energy is zero. It first scores every shift from the near
end of the range toward the far end, the near end being `low` when speeding up
and `high` when slowing down, in steps of `max(1, floor(R / 8000))`; then every
single shift within one step less a frame of the best, in the same direction.
Each pass keeps the first highest score, so a tie keeps the nearer shift.
Energies come from running prefix sums, in frame order, of each frame's squares
summed over its channels.

A splice then plays one hop crossfading from the natural continuation into the
shifted input, every channel alike, with `w = 0.5 - 0.5 * cos(pi * (k + 1) / (hop + 1))`:

```
out[c][p+k] = f32(x[c][p+k] * (1 - w) + x[c][p+s+k] * w),  k in [0, hop)
```

and continues from `p + s + hop`. Output is unchanged until a splice, so a
stretch that holds plays received audio bit for bit. While a crossfade plays,
its played share maps onto the input it shifts to for the playhead's position.

What the stretch holds is one kind of content at a time, received audio or
silence the player put there (a declared silence in the browser, a startup or
resume cushion natively), and it forgets its history where one gives way to
the other, so no splice reaches across the boundary. Its buffers are allocated
at setup; the callback never allocates, locks, or reads a host clock.

### Snaps, underruns, and silence

Before each device read, a playhead that trails the clock by more than the
larger of `bound` and one window skips onto it, dropping what the stretch holds
and the audio it passes, as video skips the frames a stall made late. One that
leads the clock by as much, as a target rise past the bound leaves it, plays
silence until the clock reaches it, dropping nothing. Less than that either way
is the stretch's to play onto. A join is the exception: until the player has
read once on the clock, its threshold both ways is one hop, so the backlog a
subscription opens on is skipped and playback starts on the clock. Only those
skips and a ring overflowing its fixed capacity drop received audio; each
counts as a skip in the player's stats. A finite tail before a declared end
plays as received, and a playhead behind the clock skips a declared silence by
its lag rather than stretching across it, which drops no audio.

After an underrun the player resumes on a cushion of one window and one read,
and never more than it starts on. The low depth it resumes on bridges, slowing
playback down while the next arrivals land. Refilling to the whole target
instead would add up to one target of silence and leave audio that much further
behind. A declared end releases the wait, so a short tail drains. The last
audio before an undeclared gap is bridged like a late frame, since nothing tells
them apart: it plays up to about a span longer than received.

`depth` counts everything received ahead of the playhead, decoded or still
encoded, in the browser. The browser holds the part of the target above its
ring's floor encoded, and a buffered player its whole lookahead. Native
playback decodes everything at once and counts only decoded PCM. The browser's
decode gate releases a playing ring's next frame while what it was given,
landed or still decoding, reaches less than its floor and one window past the
playhead, so the ring always holds a full window to stretch, and a hole in the
media plays as silence. A frame stamped within a millisecond and two samples of
where the last one ended continues it in both players: millisecond stamps, or
ones rounded from another clock, land either side of the exact boundary, which
is no hole.

A zero `stretch` turns the stretch off. Every hop holds, so received audio
plays unaltered, and the threshold is one window: a playhead trailing the clock
by more skips onto it, and one more than a window ahead of it pads silence until
the clock reaches it. Late audio is skipped, an underrun is silence, and drift
between the publisher's clock and the device's is corrected by a skip or a pad
of about one window.

### Stretch corpus

`audio-jitter/stretch/decisions.json` holds the rule's cases, each a device
rate and the four inputs with the decision they produce. Compare exactly.

`audio-jitter/stretch/corpus.json` holds the splice's cases, each a device rate
and channel count, interleaved input PCM, the decision for each hop boundary
(the last repeating), the shift each boundary took (zero for none), and the
interleaved output PCM. The input is fed as a player does, topped up to a
window before each boundary. Compare shifts exactly and output within 1e-6
absolute sample amplitude. It covers a hold, speeding up and slowing down on
a voiced signal, a bridge through its allowance and the refill after, a pace
that turns around, a narrow range, music, noise, silence, linked stereo,
stereo in opposite phase, and splices at 8, 16, 44.1, and 48 kHz.

`stretch/reference.ts` generated both, from this page and not from either
implementation, and `stretch/corpus.ts` regenerates them; the same rules as the
estimator's [corpus](#conformance-corpus) hold: it is not normative, nothing
outside the directory may import it, and `corpus.test.ts` fails when the
checked-in files no longer match it. Both implementations consume these exact
vectors; generating expected output from the implementation under test is not
conformance.

## Comfort noise

A [declared silence](#declared-silence) is not an underrun. A receiver whose
playback runs dry at a declared end counts no underrun or stall, and fills the
silence with comfort noise instead of plain silence, so a muted talker's
background carries on rather than the line going dead. It keeps doing so
through the refill before the next utterance plays.

The publisher sends nothing while muted, so the level is measured from what the
receiver itself played since playback last resumed:

```
played(samples):                  # audio just played, not comfort noise or startup padding
    if filling: filling = false; quietest = infinity; sum = 0; frames = 0
    for each frame:
        sum += every channel's sample squared
        frames += 1
        if frames == BLOCK:
            quietest = min(quietest, sum / (BLOCK * channels))
            sum = 0; frames = 0

fill(samples):
    if not filling:
        filling = true
        if quietest is finite: level = min(sqrt(quietest), COMFORT_MAX)
    each sample = uniform(-1, 1) * level * sqrt(3)
```

`BLOCK` is 20 ms at the output rate, rounded, and blocks count from the first
frame played after a silence. `COMFORT_MAX` is 0.001 RMS, -60 dBFS: audio that
never falls quiet, such as music, would otherwise fill a pause with noise as
loud as itself. `level` starts at 0, so a source that was already silent, such
as a noise-suppressed microphone, gets none, and a silence after too little
audio to fill one block keeps the previous level. Uniform noise on `[-a, a]`
has an RMS of `a / sqrt(3)`, so the noise sits at `level`. Sums are `f64` in
the order written; the noise itself is not part of conformance.

Decoded audio stops a codec's pre-skip short of the declared end when the
publisher leaves its encoder's lookahead unflushed, so the browser's ring takes
the end that early. Natively, `moq play` declares the end on the playback sink
when it reads the marker, and the stopped encoder's final frames that follow
the marker still play before the silence.

### Comfort corpus

`audio-jitter/comfort/corpus.json` holds cases of constant runs played, each
followed by the level the next silence must fill at, within 1e-9. It covers the
quietest block rather than the last, blocks straddling a level change, a stereo
mean, the -60 dBFS cap, an already silent source, and a silence that keeps or
remeasures the level.
