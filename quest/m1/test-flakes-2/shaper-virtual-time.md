# [S] moq-shaper tests on virtual time

## Goal

The `moq-shaper` unit tests pass under any host load: their verdicts come
from the seeded decisions, not from how fast the kernel delivers datagrams.
`reorder_and_jitter_overtake` and `the_seed_reproduces_the_losses` saw every
datagram lost during a slowed full-workspace run, because `round_trip` stops
at the first 200 ms of silence on a real socket.

## Plan

- Drive the tests on paused tokio time (or an injected clock) so delay,
  jitter, and the read deadline advance deterministically.
- Watch out: paused time auto-advances while the runtime idles, which can
  fire a timer before a real UDP datagram lands. If the real sockets fight
  the paused clock, separate the scheduling logic from the socket I/O and test
  it directly, keeping one socket smoke test with a generous deadline.
