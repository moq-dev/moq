# [M] Fixed-delay release holds its delay against the publisher's clock

## Goal

The [fixed-delay release](/quest/m1/tstd/delay.md) stage keeps each frame's
release at its configured delay indefinitely when the publisher's clock runs
off the receiver's by up to the ±30 ppm that ISO 13818-1 allows a system
clock. Today the release is anchored at the first frame's local arrival and
never moves, so the buffer grows or shrinks with the clock difference until
frames start missing their deadlines. The TS export and the TS passthrough
both release through it.

## Plan

Decided (2026-10-01), from a discussion with t0ms:

- Why now, against `delay.md`'s "correct it only if measured": the exposure
  is derived, and a mocked-time test shows it without waiting for a soak. At
  ±30 ppm the release drifts 108 ms an hour. A receiver whose clock runs
  slow lets the buffer grow by that much an hour, so a sink gets later and
  later; one whose clock runs fast spends the delay's margin over its
  send-ahead, and then every frame misses its deadline and is dropped. With
  a 500 ms delay and a few hundred ms of send-ahead, that is a matter of
  hours for a 24/7 channel.
- Estimate the offset between media time and local arrival with a floor
  filter: the minimum offset over each window, so that queueing and
  retransmission delay, which only ever add, do not move the estimate. The
  drift is the slope of those floors across windows. Media time is the DTS
  for the demultiplexed export and the PCR for passthrough; the stage does
  not care which.
- Steer the release clock towards that estimate within 13818-1's own limits
  on a system clock: within ±30 ppm of nominal, and changing by no more than
  0.075 Hz/s at 27 MHz. A source outside them is counted and surfaced in the
  export's stats, not silently followed. Its margin then erodes, and its late
  drops are attributed to drift.
- The PCR values the export writes do not change. Only the wall-clock pace
  of the bytes moves, by at most the steering limit, so a receiver
  recovering its clock from them sees a clock inside the standard's
  tolerance.
- A rewind (a new program generation) re-anchors as it does today, and
  restarts the estimate.

Test with mocked time: a source at +30 ppm and one at -30 ppm over a
simulated 24 h lose no frame to lateness, and hold the release within a
stated bound of the configured delay once converged (proposed: 10 ms). A
source at 50 ppm saturates the steering, and is counted as out of tolerance.
A step in queueing delay does not move the estimate. The slew stays within
0.075 Hz/s throughout. A wall-clock soak against a real drifting source is
the end-to-end gate, run by the [T-STD line](/quest/m1/tstd/README.md).

Public API: the release stage gains the estimator; its stats gain the drift
estimate and an out-of-tolerance count. Wire: none.

## Required

- [Fixed-delay release](/quest/m1/tstd/delay.md) - the stage this steers

## Related

- [TS passthrough](/quest/m1/ts-passthrough.md) - releases on the source PCR through the same stage
