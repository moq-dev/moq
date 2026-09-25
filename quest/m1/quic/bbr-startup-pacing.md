# [S] Recalibrate BBR startup pacing from measured RTT

## Goal

The first available measured RTT replaces BBR's nominal 1-ms startup
pacing estimate. A media sender that stays application-limited does not keep
an inflated pacing rate for its entire session.

## Plan

The audited 12-KB initial window retains a 33,276,000-byte/s pacing rate
after a measured 10-ms RTT. Startup only raises its rate, and an
application-limited flow need not leave Startup. [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L443)
reinitializes pacing once an RTT measurement becomes available.

Review and reuse [upstream PR #802](https://github.com/n0-computer/noq/pull/802)
if still applicable, preserving its author's attribution rather than
reimplementing it. Check the actual ACK/RTT callback order: the configured
initial RTT is not evidence of a measurement, and the first ACK can precede
the transport RTT update. Recalculate the send quantum consistently.

Add CI regressions for measured RTTs above and below 1 ms, a continuously
application-limited source, and a secondary path initialized after the
handshake. Preserve subsequent bandwidth-driven Startup growth. Report any
public RTT-estimator API change and document it inline; no wire change is
intended.

## Related

- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - deliver the corrected controller to MoQ
- [Upstream the fork](/quest/m1/quic/upstream.md) - offer general fixes upstream
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - measure the corrected controller on media traffic
- [noq #800](https://github.com/n0-computer/noq/issues/800) - existing upstream report; do not duplicate it
