# [M] Controllers hear an ACK's losses before the ACK

## Goal

moq-quic reports the packets an ACK declares lost before it reports the ACK,
as RFC 9002's OnAckReceived orders them, so a congestion controller's loss
response takes effect on the same ACK, for every controller.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan): ✅ the transport reports losses
before the ACK (rejected: only refresh BBR3's pacing and cwnd at the end of
`on_loss`, which leaves the double cut).

Today `Connection::on_ack_received` (`rs/moq-quic/src/connection/mod.rs`)
calls `Controller::on_ack` and then `detect_lost_packets`, an order inherited
from quinn. With BBR3, a loss that ends ProbeBW_UP leaves pacing at 1.25x
and UP's window until the next ACK, since `on_loss` recomputes neither, and
losses found by the ACK that closes a loss round open a second round, so
DOWN, CRUISE, or Drain can cut the short-term model twice where Linux cuts
once ([tcp_bbr.c:2176-2201](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L2176-L2201)).

Change the order for all controllers (Cubic, SCReAM, and GCC included),
document it on `Controller`, and drop BBR3's note on the old order around
`on_spurious_loss`. A loss timer that fires with no ACK still needs BBR3's
`on_loss` to refresh pacing and cwnd. Test both effects. No single PR caused
it: the order is quinn's.
