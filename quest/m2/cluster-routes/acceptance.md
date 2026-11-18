# [M] Route layer acceptance

## Goal

A lite-07 subscriber whose grant permits more disjoint regions than one
ANNOUNCE_REQUEST can name either discovers every region or says it cannot:
no reader sees a region it never asked for as synced and empty.

## Plan

Decided 2026-11-14 (maintainer, /quest-complete on
[moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768)): ✅ stays in
m2 and gates the cut, per lite07-finalize's 2026-10-13 rule that routes'
tier and partition tests run before the cut (rejected: moving it to m3 and
fixing late routing flaws in lite-08). A flaw found here is fixed on
lite-07-wip before it freezes.

[moq-dev/moq#9112](https://github.com/moq-dev/moq/pull/9112) confined the
`lite06_churn` mitigations to routes learned over a hop chain, made a claim
the route node lets go of leave every copy at once, recorded why lite-06
keeps its `Start, End, Start` residual, and pointed the Node ID 0 coverage
at the native JS claims lanes of `just test interop`. What remains:

`auth::residual_lite_07_a_grant_too_wide_to_ask_syncs_without_its_last_region`
pins the flaw on real sessions. A relay grants its client 18 disjoint
regions over two tokens; each token's grant fits its own AUTH message, but
the union does not fit the one ANNOUNCE_REQUEST a subscriber's whole set
must (`lite::request_fits`, which the publisher enforces). `coarsen` in
`lite/subscriber/routed.rs` then drops the last region, and `release` treats
a want no asked prefix overlaps as answered: a whole-tree reader syncs
without that region's broadcast, and a reader of the region alone syncs
empty. Fix it per the decision under Waiting, and turn the residual into
the passing case.

## Waiting

- Maintainer: how a subscriber handles a grant too wide for one
  ANNOUNCE_REQUEST. (A, recommended) Wire: a subscriber's set may pass one
  message when each prefix past it is a region the publisher granted, so the
  publisher's own grant bounds what it holds and the client discovers
  everything; changes the lite-07-wip draft and both publishers, `rs/moq-net`
  and `js/net`. (B) API: a reader of an unasked region learns the session
  cannot answer it, a public change in Rust, JS, and the bindings. (C)
  Neither: a reader overlapping an unasked region never syncs, a hang in
  place of a wrong answer. Done once the choice is recorded in Plan.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the contract and its fixtures
