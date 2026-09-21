# [L] moq export ts: the audio/video interleave is decided by arrival timing, so two exporters of one broadcast render the same media in different orders

## Goal

Implement and verify the behavior tracked in [#2829](https://github.com/moq-dev/moq/issues/2829)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Two `moq export ts` processes subscribed to the same broadcast do not emit the same frames in the
same order. `pick_next_track` takes the smallest timestamp among tracks that *currently hold* a
pending frame, and a track only holds one once its own `poll_read` has returned  -  so when audio for
timestamp *t* has not yet arrived but video for *t+1* has, the exporter emits the video. Which frame
leads is therefore a property of when bytes reached that process, not of the media.

This is the last of three values the exporter mints from process state rather than from the
broadcast. #2779 covers the continuity counters and #2825 the SI cadence; with both addressed, this
is what remains, and unlike the other two it moves whole PES packets rather than renumbering a field.

#### Measurement

One publisher, one relay, two `export ts` subscribers of the same broadcast, each paced to a
constant-rate RTP leg whose packet placement is derived from the stream position rather than the
process clock. The two legs are then compared at equal stream slots with the continuity counter
masked, so what is left is only what the exporters actually rendered differently.

Single-track source (H.264, no B-frames, no audio, 2 Mb/s CBR), second subscriber joining 20 s late:

| build | slots identical, counter masked |
|---|---|
| `main` | 96.43 % |
| #2825 | **100.00 %** |

Multi-track source (H.264 with B-frames, two audio renditions, SDT and NIT), same rig:

| build | slots identical, counter masked |
|---|---|
| `main` | 87.75 % |
| #2825 | 89.72 % |
| #2825 with the monotonic `due` from the review | 95.62 % |

The residual 4.4 % is not a late-join effect. With both subscribers started at the same instant it is
4.82 %. Over 62 073 compared slots the two legs agree exactly on every table  -  PAT 111/111, PMT
111/111, SDT 22/22, NIT 5/5  -  while rendering different numbers of media packets:

```
    0x006f (video):  263,264 / 263,283   (+19)
    0x0079 (audio):    7,313 /   7,241   (-72)
    0x007b (audio):    5,789 /   5,738   (-51)
```

That is the signature of an ordering difference rather than a numbering one: the same media, placed
in different slots, so every byte after the first divergence lines up against something else.

#### Mechanism

`poll_next` fills `track.pending` for whichever tracks have a frame available right now:

https://github.com/moq-dev/moq/blob/6d3c51d72/rs/moq-mux/src/container/ts/export.rs#L254-L283

and then picks the minimum over exactly those:

https://github.com/moq-dev/moq/blob/6d3c51d72/rs/moq-mux/src/container/ts/export.rs#L682-L688

The pick itself is deterministic  -  `(timestamp, pid, name)` breaks every tie  -  but the *candidate set*
is not. A track whose frame has not arrived is simply not considered, so the exporter emits the
earliest available frame rather than the earliest frame. Two processes with different arrival timing
therefore choose differently, and neither is wrong in isolation.

#### Why it matters

An MPEG-TS receiver does not care about the interleave. A *second* receiver does: SMPTE ST 2022-7
seamless protection switching merges two copies of one stream by taking whichever arrives first,
which requires the two copies to be the same bytes. With the counter and the cadence fixed, a pair of
legs is already byte-identical on single-track content; on ordinary multi-track content the interleave
is what stops it, and no amount of care downstream can put the frames back in a common order.

It also affects anything that compares two renderings for equality  -  regression tests across runs, and
checksums of an exported stream.

#### Suggested direction

Emit only once every unfinished track has a pending frame, so the minimum is taken over the whole
candidate set rather than the arrived part of it. That is a bounded wait in practice  -  the tracks of
one broadcast advance together  -  but it should be bounded explicitly so a stalled or genuinely idle
track cannot hold the output: after a small window, emit what is in hand, as today.

That makes the ordering a function of the media wherever the window is not exceeded, which is the
condition under which two legs can be a redundant pair. It would fit naturally alongside the existing
`--max-age`.

I am happy to put this behind a flag if a deterministic-rendering mode is preferable to changing the
default.

## Closes

- [#2829](https://github.com/moq-dev/moq/issues/2829) - close this issue when the quest finishes
