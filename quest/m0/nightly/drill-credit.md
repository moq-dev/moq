# [M] Claimed fetches wait for credit

## Goal

`bursts_cross_a_cluster`, `bursts_cross_a_transport_peer`, and
`bursts_cross_a_flapping_peer` (`::impaired`, `rs/moq-relay/tests/drills.rs`)
deliver every group again, and drill-sensitivity's
`ingress-burst-never-collected` baseline passes unmutated.

## Plan

Broken by [moq-dev/moq#6409](https://github.com/moq-dev/moq/pull/6409)
("refuse native requests without stream credit"), reproduced on `main`: the
drills panic at `drills.rs:1305` with groups lost as
`Failed("no stream credit")`. A relay's claimed FETCH under a burst is now
refused when the peer's stream credit is exhausted, where it used to wait.

Find which hop refuses and decide by behavior, not by the test: a request a
relay makes on a subscriber's behalf should queue for credit (backpressure)
rather than fail, unless the refusal is the bound #6409 meant to add against
a hostile peer. Keep that bound; never loosen the drill's assertion.

Public API: none expected. Wire: none.
