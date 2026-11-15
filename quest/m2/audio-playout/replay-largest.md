# [S] Replay the publisher's largest group at tune-in

## Goal

The audio-quality replay measures the join's backlog the way a live player
does. Each recorded trace carries the publisher's largest group as the
subscription began, and the replay's consumer learns that number as a wire
subscriber learns it from the publisher's answer. `relay-mic-rtt40`'s target
p95 then falls from 320 ms to 120 ms, and its budget row tracks the consumers'
backlog reports, so a regression there fails the row.

Boundaries: the native decode path's replay is still owed separately (see the
epic's README). No API is added to `@moq/net` or the container consumer for the
replay's sake, and the checked-in recordings are not re-recorded.

## Plan

Since [moq-dev/moq#8761](https://github.com/moq-dev/moq/pull/8761), both
consumers report a group they skip unread as join backlog when its sequence is
at or below the publisher's largest group at subscribe time. The replay
(`js/watch/src/audio/replay.ts`) writes a trace into a local
`Track.Producer`, whose largest comes from the first group to land. For
`relay-mic-rtt40` that is the stale tune-in frame, group 7499, below the
backlog the consumer closes, so the replay reports nothing and still measures
320 ms.

Decided 2026-11-15 (maintainer: plan it in m2, under this epic). The rest was
settled from the trace and a local measurement:

1. **The trace carries it.** `Trace` (`test/audio-quality/clients/js/src/schema.ts`)
   gains a required `largest: number`, the publisher's largest group as the
   subscription began, and its `version` goes to 4. Every checked-in trace gets
   one, so no trace means "the track's own" by leaving it out.
2. **New captures record it.** The capture observer
   (`test/audio-quality/clients/js/src/capture.ts`) reads its subscription's
   `largest()` the way `js/hang`'s consumer does: once the track's info or
   first group shows the subscription established and `start()` resolved, so a
   cached group racing the answer, or live groups landing after it, don't set
   it. `record.ts` writes it into the trace.
3. **Checked-in traces derive it.** None of them recorded the subscribe
   answer, and re-recording would need a person at the MacBook's microphone and
   would move every other column of the baseline. Each gets the newest group
   whose media timestamp plus the path's lowest lag (the smallest
   `at - timestamp` over the trace) falls at or before the first arrival: the
   live edge as the first frame arrived. Its `source` names the derivation.
   For `relay-mic-rtt40` that is group 8994. The backlog the relay held
   reaches about 9047, and the live frames after it settle at that lag from
   about 1.3 s in.
4. **The replay answers it.** The replay hands its consumer a subscriber whose
   `largest()` answers the trace's `largest` first, as a wire subscriber's does
   once the publisher answered, then each newer group as it lands. It
   delegates everything else to the local subscriber. Rejected: creating an
   empty group head at `largest`, which delivers a group the recording never
   saw and moves the start; a consumer option, which is API for a test.
5. **Budgets follow.** Replay budgets equal what they measure, so every row
   that moves is re-budgeted in the same PR, and the note in `budgets.json`
   loses its sentence about the stale tune-in frame. Measured locally on
   #8761's head with `largest` 8994 injected, `relay-mic-rtt40` reads target
   p95 120 (from 320). It also moves underruns/min 0.3 to 0.7, player skips/min
   0.3 to 0, trimmed ms/min 83 to 0, accelerated ms/min 88.3 to 97.7, A/V lag
   p50 0.6 to 2.3 and p95 6.9 to 8.6, and recover ms 250.7 to 749.3: the
   trade #8761's pick 3 accepted. Group 9047 measured the same row, so the
   derived number is not on a knife's edge.
6. **Docs.** `test/audio-quality/README.md`'s Traces section documents the
   field, how a capture records it and how the old traces derive it.

## Related

- [moq-dev/moq#8761](https://github.com/moq-dev/moq/pull/8761) - the consumers' backlog reports this replay measures
