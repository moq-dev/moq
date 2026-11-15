# [S] BBR3 counts Startup loss events per lost packet

## Goal

An application-limited media sender in Startup whose bottleneck shrinks
leaves Startup on high loss, as draft-07 specifies, and moq-congestion's
`Lost` drops the `new_burst` field that only the old counting needed.

## Plan

`note_loss` (`rs/moq-congestion/src/bbr3/mod.rs`) counts discontiguous lost
ranges, as draft-06 read, from `Lost::new_burst`, which moq-quic computes.
[draft-07](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-07.html#section-5.3.1.3)
counts every lost packet and resets the count in
`AdvanceLatestDeliverySignals`, `ResetCongestionSignals`, and spurious-loss
undo; ours resets it in none of the last two. A media sender bursting about
14 packets per frame into a tail-drop link that shrank (50 to 3 Mbit/s)
loses one run per frame, one to three events per round, never six, and its
application-limited rounds block the plateau exit, so Startup never ends
and keyframe tails keep dropping.

Count per lost packet, reset where draft-07 does, remove `Lost::new_burst`
and its computation in moq-quic, and replace the even 1-in-25 drop test with
a tail-drop bottleneck test.

In m2, apart from [its epic](/quest/m1/bbr3-references/README.md):
removing `Lost::new_burst` changes moq-congestion's public API, which the
cut publishes for the first time. Decided 2026-11-15 (maintainer,
/quest-complete): ✅ keep it in m2 (rejected: m3, keeping the API).
Public API: removes `Lost::new_burst`. Wire: none.

## Related

- [BBR3 matches its references](/quest/m1/bbr3-references/README.md) - the rest of the comparison's fixes
