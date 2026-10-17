# [S] The relay burst drill holds on an impaired peer link

## Goal

The `bursts_cross_a_transport_peer::impaired` drill
(`rs/moq-relay/tests/drills.rs`) passes reliably, fixed at its cause. It
still proves that bursts cross a relay peer link over an impaired transport
without the subscription aborting while the link is up.

## Plan

Seen 2026-10-16 on the io_uring job for #5772 (run 38974661047, head
1c1055c): the subscription aborted with "internal error" at the drill's
`Err(err) => panic!("the subscription aborted: {err}")` arm, which is
reached only when the route is not flapping. A rerun passed. The PR touched
only the dev shell, so the drill ran only because every crate was retested.

Decided 2026-10-16 (maintainer): a child of this questline, m1. Reproduce it
first, on the io_uring lane under load. Then find which layer produces the
"internal error": an abort that reaches a subscriber while its peer link is
up is a product bug unless the drill's impairment legitimately cut the link,
in which case the drill must classify that as a flap. Never widen a timeout
or add a retry.

Public API: none expected. Wire: none.
