# [S] Explain the extra steady lag on a warm peer link

## Goal

The extra steady-state lag seen on a warm cross-region peer link is either
explained and fixed, or shown to be host load. In some of #9329's runs a
16 Mbps track settled about 385 ms behind live after catching up, against
about 145 ms in the rest.

## Plan

Decided 2026-11-22 (maintainer, /quest-plan): ✅ plan it (rejected: drop it
as host load).

Seen on [moq-dev/moq#9329](https://github.com/moq-dev/moq/pull/9329): a
temporary test on `rs/moq-relay` `receive_credit.rs`'s harness with
`moq-shaper`, 125 ms each way, a bottleneck behind a 250 ms drop-tail queue
(bufferbloat, no random loss), and 32 MiB pushed through first to warm the
link. Some warm runs, with either a 1 MiB or a 64 MiB stream window, settled at
about 385 ms instead of about 145 ms. A 16 Mbps flow cannot fill that queue
alone, so the extra 240 ms comes from somewhere else: the shaper, a
send-side buffer, pacing, or the host. The host was heavily loaded and the
test was not committed.

Rebuild the measurement and run it on a quiet host first (`nixos` is the
always-on one). If the lag reproduces there, find which buffer holds it and
fix that. If it does not, record that and delete this quest. Commit the
measurement as a test or bench if it finds a real cause, so it guards the fix.

## Related

- [Peer links default to release's windows](https://github.com/moq-dev/moq/pull/9329) - where the lag was measured
