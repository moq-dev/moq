# [S] Protect bandwidth samples during BBR ProbeRTT

## Goal

Packets deliberately rate-limited by ProbeRTT cannot lower the bandwidth
model as if they measured a network capacity reduction. Normal sampling
resumes after the protected delivery interval.

## Plan

[handle_probe_rtt](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/congestion/bbr3/mod.rs#L1182) omits the application-limited marker present
in [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L920) and
[draft section 5.3.4.3](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.3.4.3). The audited controller
sends unmarked packets during ProbeRTT even with a backlogged application.
QUICHE's BBR3 ProbeRTT also lacks an explicit marker; the requirement here
is the draft/Linux behavior, not universal Google parity.

Reproduce that unmarked send, then test entry, the full ProbeRTT interval,
exit, and delayed ACKs for packets sent during the interval. Verify reduced
samples do not replace a learned capacity solely because ProbeRTT reduced
the window, while a legitimate higher sample can still raise the model.
Do not mark the connection application-limited forever or change the
ProbeRTT policy merely to mask a sampling bug. Add the regressions to the
fork's CI; keep the fix internal with no wire change.

## Required

- [Fork noq](/quest/next/quic/fork.md) - fixes and CI regressions land in moq-dev/noq

## Related

- [Release BBR fixes](/quest/next/quic/bbr-release.md) - deliver the corrected controller to MoQ
- [Upstream the fork](/quest/next/quic/upstream.md) - offer general fixes upstream
- [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) - measure the corrected controller on media traffic
