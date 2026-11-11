# [L] Seeded lite-07 meshes hold the route rules and an announce budget

## Goal

A seeded sweep of random lite-07 meshes, real sessions over the memory
transport on a mocked clock, checks the route model's rules on the
implementation at every quiet point and counts announce messages against a
budget. Nightly sweeps fresh seeds, and a failing seed replays bit-exact.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan). The route model (the
`verify-route` skill) checks every ordering on meshes of up to four relays and
samples a chorded ring of five; this checks the real stack, sessions included, on larger random
ones. `rs/moq-net/tests/integration/mesh_withdraw.rs` already sweeps 200
seeded meshes, but on lite-06; [route layer
acceptance](/quest/m2/cluster-routing/routes/acceptance.md) ports specific
fixtures, not a random sweep.

- Each case: a random mesh of relays (sizes swept) with clients, publishing
  and watching broadcasts; random link flaps, relay restarts, origin restarts
  under a fresh identity, and client joins and leaves.
- At every quiet point, assert the `verify-route` rules on what each client
  and relay sees: truthful, no revival, no false end, no regression, loop-free,
  and settled. Count ROUTE and announce messages per external change against
  the same budget the model states, and report the counts as a benchmark swept
  over mesh size and churn. The model's `Budget`: from one quiet point to the
  next, the ROUTE, batch, ROUTE_REQUEST, and echo messages sent (the Sim's
  `route_msgs + claim_msgs + requests`) stay within 4 per direction of each
  configured session per external change made in that span (a claim change, a
  session open or close, a collection).
- Seeds follow the repo's `MOQ_*_SEED` convention; the merge gate runs a few
  fixed seeds only if they stay fast, and Nightly sweeps fresh ones.

Public API: none. Wire: none.
