# [L] Native probing in moq-quic

## Goal

While a moq-quic sender is app-limited below its session's probe target, it
retransmits in-flight STREAM data (sent, unacknowledged, and not yet expired)
to fill BBR3's ProbeBW_UP phase, so that phase's sample isn't app-limited and
the estimate rises when the path has room. Media frames keep their deadlines,
redundant bytes stay within budget, and unique goodput is counted once.
Sessions on moq-tokio and moq-uring then advertise PROBE `Increase`. A
measured no-go is a valid outcome.

## Plan

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)), replacing the old plan's "measure which
mechanism wins first" step:

- **Retransmit in-flight data rather than pad.** It needs no wire change, and
  a duplicate is useful if the original was lost. Duplicates still delay
  useful bytes and are not free: keep every byte in congestion accounting and
  keep wire capacity distinct from application goodput.
- **Ride BBR's own cycle.** Only during ProbeBW_UP, fill the extra pacing its
  1.25x gain allows above the media, capped by the target; BBR sets the
  cadence. Rejected: a burst schedule of our own, which would fight the
  controller it is trying to inform.
- Never delay ready frames or manufacture keyframes to probe. Stop on
  persistent queue growth or congestion evidence. Don't force Startup exit or
  replace capacity with the encoder rate because the source plateaus. Keep
  current goodput, historical capacity and its age, and the desired rate
  distinct; preserving an old estimate does not discover capacity.
- Evaluate the IMMEDIATE_ACK extension so ACK delay doesn't obscure a short
  sample.
- Implement the [target](/quest/m2/probe/target.md) quest's sender hook in
  moq-quic's recovery and pacing layer and the moq-tokio and moq-uring
  adapters. A controller with no ProbeBW_UP phase can't, so its sessions
  fall back to [padding](/quest/m2/probe/padding.md).

Acceptance, in moq-quic's seeded simulation (`rs/moq-quic/src/tests/`,
extending `Bottleneck` to step its rate) and wired into CI at least nightly:
an illustrative 2 Mbit/s source seeking 4 Mbit/s while the path steps 3 to 12
to 3 Mbit/s, on identical media traces and wire budgets. Cover keyframes, idle
restart, stale high estimates, receiver credit limits, compressed ACKs, and
competing CUBIC and BBR flows. Add a congested arm below the media bitrate
behind a deep but finite queue for impairment acceptance and performance.
Verify actual overflow drops, adding competing loss-based traffic if BBR
alone will not fill it. Sweep queue depth separately. Random or scripted burst loss may
prove recovery correctness, but never gates performance. Set the
deadline, overhead, and fairness budgets before tuning. Report validated
upward adaptation, downward reaction, estimate age, frame deadlines, queue
delay, unique goodput, and redundant bytes, against a no-probing baseline. A
successful small burst is not proof of full path capacity.

On a no-go, keep the baseline, record why, and leave padding as moq-quic's
sender; the [viewer up-switch](/quest/m2/probe/viewer-upswitch.md) then
requires padding instead of this. Padding can't reach moq-transport drafts 14
to 17, so sessions on those drafts would keep `Report`.

## Required

- [Session probe target](/quest/m2/probe/target.md) - the target this probes to and the sender hook it implements

## Related

- [Per-stream deadlines](/quest/m2/quic-deadline.md) - decides when in-flight data has expired
- [BBR media study](/quest/m2/quic-bbr-natural-drain.md) - separate ProbeRTT policy experiment
