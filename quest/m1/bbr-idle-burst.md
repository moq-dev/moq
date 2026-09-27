# [S] BBR idle burst

## Goal

After a long keep-alive-only idle, a BBRv3 (`delay`) sender paces its next
burst near the bandwidth it learned before, not at one packet per RTT. A
regression test proves it.

## Plan

- The likely fix already shipped: moq-dev/noq#5 tells the controller of
  starvation before the next send, released in moq-noq 1.3.1 and pinned by
  #4206. The reporter ran 1.3.0. Nobody has reproduced the stall on either.
- In the fork, add a virtual-time transport test: learn the bandwidth, run
  keep-alive only for about five minutes, send 250 KB, and assert the pacing
  rate stays at or above about 0.9x the earlier max bandwidth. It must fail on
  1.3.0 and pass on 1.3.1. The existing label tests do not check the rate.
- If 1.3.1 still stalls, find the remaining cause (a stale `bw_shortterm` or a
  ProbeRTT effect after idle) and fix it in the fork. Dropping the estimate
  after long idle is a policy change for the m2 study, not this quest.
- The `iroh` feature uses upstream noq, which lacks the fix; offering it there
  belongs to the upstream quest.

## Closes

- [#4219](https://github.com/moq-dev/moq/issues/4219) - the first send after an idle period is paced at a trickle

## Related

- [Upstream the fork](/quest/m1/quic/upstream.md) - offer the starvation fix to n0-computer/noq
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - broader media measurements
