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

What the publishers do today with an ask wider than the grant (traced in
code, then run on a server enforcing `r00/a/**` and `r01/b/**` with the
subscriber forced to ask for the root):

- The ask is legal. The draft already says a prefix outside the
  subscriber's scope "lists nothing and is SYNCED at once"
  (`doc/draft/moq-lite.md`, Announce). Confining the ask to the grant is
  only `coarsen`'s choice, from
  [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987). Neither
  publisher checks an ask against the grant: both filter each claim by its
  path (`Answer::shows` in `lite/publisher/routed.rs`, `#changes` in
  `js/net/src/lite/answer.ts`), and `Answer::poll_permit` re-filters every
  claim the node told as the grant changes. The root ask got ANNOUNCE_OK,
  SYNCED, both granted broadcasts, nothing outside the grant, and no close.
- A claim above a granted region is lost. A publisher presents a broader
  claim at the requested prefix itself (`relay.rs`, "presented at the
  prefix itself"). Asked for a region, a service claim at `r00` arrives at
  `r00/a`, which the grant covers. Asked for the root, it would arrive at
  `r00`, which the grant does not cover, so the filter drops it. The reader
  syncs without a claim that serves its region.
- `js/net`'s subscriber already asks for the root once its readers' set
  does not fit (`#wanted` in `js/net/src/lite/routed.ts`), ignoring the
  grant. So a confined JS client with that many readers has this gap
  against either publisher today.

## Waiting

- Maintainer: how a subscriber handles a grant too wide for one
  ANNOUNCE_REQUEST. Recommended: D.
  - (D) Ask wider, answer the granted part. `coarsen` falls back to the
    regions' longest shared ancestor, up to the root, and never drops a
    region. Draft: a requested prefix wider than the subscriber's scope is
    answered as if each granted region inside it were asked: a claim above a
    region is presented at that region, and SYNCED follows every region's
    initial set. Add one ordering rule: a publisher answers with a grant
    change before the ANNOUNCE_OK of the next edit it reads after the
    AUTH_OK, so a subscriber whose grant widens under a wide ask can send an
    empty edit and hold new readers until its ANNOUNCE_OK. `rs/moq-net`:
    the publisher reads each granted region inside an ask through its own
    node key, as it reads mounts, re-narrowed on each grant change. That
    also keeps the publisher from reading ungranted subtrees, which bounds
    its work and leaves SYNCED's timing blind to what it hides. The
    subscriber's `coarsen` widens instead of dropping. `js/net`: the answer
    presents a claim above a granted region at the region; the subscriber
    already falls back to the root, and gains the widening sync. No message
    field changes and no public API changes. It also fixes JS's existing
    root fallback.
  - (A) Exception to the size rule. A subscriber's set may pass one message
    when each prefix past it is a region the publisher granted. Draft: the
    exception, plus how both ends derive a grant's regions from its patterns
    (`interest_prefixes`, wildcards included), plus what happens when a
    narrowing (an AUTH_OK replacing the grant, an AUTH_ERROR, or a lapse)
    crosses an edit already sent. The acceptor judges a request when it
    reads it, so that edit names a region no longer granted, and as written
    the publisher must close the session. A narrowing between edits leaves
    the set past the limit until the subscriber's next edit removes the
    region. `rs/moq-net`: the publisher checks each extra prefix against its
    grant, and `coarsen` keeps every granted region. `js/net`: the same
    check, and its subscriber, which knows nothing of the grant today, has
    to ask by the grant's regions.
  - (B) API: a reader of an unasked region learns the session cannot answer
    it, a public change in Rust, JS, and the bindings.
  - (C) Neither: a reader overlapping an unasked region never syncs, a hang
    in place of a wrong answer.

  D over A: D keeps the one-message rule whole, never names a region on the
  wire, so a narrowing cannot turn an honest edit into a violation, and
  needs no shared rule for deriving regions. It costs a presentation rule
  and the publisher's narrowing, which JS's root fallback needs anyway. Done
  once the choice is recorded in Plan.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the contract and its fixtures
