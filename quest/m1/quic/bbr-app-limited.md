# [S] Prove a BBR burst after a long idle is paced at the learned bandwidth

## Goal

After minutes of keep-alive-only idle, a BBRv3 (`delay`) sender paces its
next burst near the bandwidth it learned before, not at a trickle, and a
regression test in the fork proves it.

## Plan

moq-dev/noq#5 (in moq-noq 1.3.1; main pins 1.3.2) fixed the label bug this
quest was opened for. The transport calls `Controller::on_app_limited` on
every empty poll that nothing held back, and BBR marks starvation before the
next send. Its tests cover streams, datagrams, batched ACKs, and backlogs
held by the window or the pacer. What is left is
[#4219](https://github.com/moq-dev/moq/issues/4219), reported on 1.3.0 and
not yet reproduced on either version: the first 250 KB after five idle
minutes took 5.2 s at 50 ms RTT, against 0.5 s with CUBIC.

The fix plausibly covers it. The fork's max-bw filter ages only on
non-app-limited samples, and before #5 every other keep-alive was stamped
non-app-limited. Nothing measures it, though. Add a virtual-time transport
test in the fork: learn the bandwidth, idle on keep-alives for about five
minutes, send 250 KB, and assert the pacing rate stays at or above about
0.9x the earlier max bandwidth. Check that it fails on 1.3.0.

If it still stalls, find the remaining cause, such as ProbeRTT entered during
the idle or a stale `bw_shortterm`, and fix it in the fork. Dropping the
estimate after a long idle is a policy change for the m2 study, not this
quest.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - lands in `moq-quic` on `dev`, not the frozen fork

## Closes

- [#4219](https://github.com/moq-dev/moq/issues/4219) - the first send after an idle period is paced at a trickle
