# [S] Align BBR loss handling with draft-06

## Goal

BBR's loss path follows draft-06 and Google Linux: a lost packet is judged
from its own delivery state, not from the last ACK's sample. The shipped
spurious-loss undo, which re-enters ProbeUp through Refill, keeps working.
The controller's spec citations point at draft-06.

## Plan

The lost-packet sample gap remains after the seven correctness fixes in
`moq-quic`'s BBR3 (`rs/moq-quic/src/congestion/bbr3/mod.rs`):

- Loss handling reuses the shared ACK sample (`self.rs`) as scratch space,
  and skips the inflight-too-high check when a loss arrives before any ACK
  sample exists. [Google Linux](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c)'s
  `bbr_handle_lost_packet` builds a local sample from the lost packet instead. Do the same so a loss
  never reads or clobbers ACK state.

Release 1.3.1 already restores a spurious ProbeUp exit through Refill, as
[draft-06 section 5.5.11](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.5.11)
requires. Preserve that behavior and its regression.

While in the file, move the remaining draft-05 links and renamed pseudocode
identifiers (such as `probe_up_acked_per_inc`) to draft-06; comment-only.

Decided in the 2026-10-05 audit: this waits on the hard fork, which the
maintainer prioritizes, and lands in `moq-quic`'s BBR3, not in the frozen
moq-dev/noq. Add failing regressions on the shared test `Sim`: a loss before the
first ACK sample, and a loss between ACKs that must not alter the next ACK's
sample. Retain the spurious-loss undo coverage through Refill. Internal only;
no `Controller` or wire change.

Decided in the 2026-09-30 audit: moved to m2. The seven correctness fixes
already shipped, and this remaining gap has no observed impact on MoQ
traffic yet.

## Required

- [BBR ACK cleanup](/quest/m1/bbr-ack-cleanup.md) - edits the same `bbr3/mod.rs` packet bookkeeping and lands first; this keeps the metadata it preserves

## Related

- [Switch](/quest/m1/quic/fork/switch.md) - MoQ runs on `moq-quic`; not a blocker, since this is a local `rs/moq-quic` fix (decided for the m1 BBR quests in fork/input-validation)
