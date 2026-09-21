# [M] Preserve QUIC packet identity in BBR

## Goal

BBR associates every send, ACK, and loss with the correct packet across
Initial, Handshake, and application-data spaces. Reused packet numbers never
alias or invalidate an ordered lookup.

## Plan

The audit of noq-proto 1.3.0 and upstream `1a26a8b` found separate internal
packet queues, but [every public Controller callback forces Data](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/congestion/bbr3/mod.rs#L1881).
Sending Initial packets 0 and 1 followed by Handshake packet 0 makes an ACK
for the Handshake packet select Initial packet 0 instead.

Fix identity at the transport/controller boundary in moq-dev/noq. Cover
coalesced sends, repeated packet numbers across spaces, losses, key discard,
and path ownership through the real transport callbacks. Private helpers
that accept a space while the public path discards it are insufficient.

Land a regression that sends the overlapping sequence above and verifies the
ACK uses the Handshake packet's send time and delivery snapshot. Check every
controller consumer if the public Controller contract changes, including
other controllers and custom implementations. Settle any public API change
with the maintainer before implementation; document it inline. No QUIC wire
change is intended. Run the regressions in the fork's CI.

## Required

- [Fork noq](/quest/next/quic/fork.md) - fixes and CI regressions land in moq-dev/noq

## Related

- [Release BBR fixes](/quest/next/quic/bbr-release.md) - deliver the corrected controller to MoQ
- [Upstream the fork](/quest/next/quic/upstream.md) - offer general fixes upstream
- [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) - measure the corrected controller on media traffic
