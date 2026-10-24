# [M] Encrypted broadcasts serve undeclared track names

## Goal

An encrypted broadcast producer answers requests for track names it never
created or reserved, as a plaintext `moq-net` broadcast does. Its request
handler (`dynamic()` today, `requests()` after
[named requests](/quest/m1/api-audit/requests.md)) mirrors moq-net's and
yields requests named by their semantic name, so a handler such as
moq-stats' or moq-transcode's serves an encrypted broadcast unchanged. A
request whose physical name does not open under the generation is answered
as missing and never reaches the handler.

## Plan

The design is the [E2EE epic](/quest/m2/e2ee/README.md)'s Dynamic names section.

- Mirror moq-net's handler methods exactly, so
  [moq-e2ee implements the traits](/quest/m2/net-wrap/e2ee.md) can adopt
  it. The wrapper opens each net request's name, claims it as
  `reserve_track` does, and hands the handler a request whose `name()` is
  semantic. Return the semantic name from the requests `reserve_track` makes
  too, so the two never disagree.
- A refused name (forged, from another epoch, or malformed) costs one open
  and holds nothing: no claim, no derived key, no queued request. Test a
  relay-forged name, an older epoch's name, and a burst of forged names
  that leaves no state behind.
- Claims stay permanent, so a name served and then dropped is refused when
  requested again in the same generation, as `create_track` refuses it.
  Check that this suits a handler that serves on demand and drops idle
  tracks; if it does not, propose the fix in the PR rather than releasing a
  claim.
- Listing: decide whether the wrapper's handler offers moq-net's `lister`,
  which would take semantic names and list their physical names, or leaves
  it out until a consumer needs it.
- Rust only: `@moq/net` has no public broadcast request handler, so no JS
  writer serves undeclared names yet. Recovery still lands in both cores
  through [reversible names](/quest/m2/e2ee/reversible-names.md).

Public API: a request handler on `moq_e2ee::broadcast::Producer`, and
`track::Request::name()` returns the semantic name. Wire: none.

## Required

- [Reversible physical names](/quest/m2/e2ee/reversible-names.md) - the recovery this handler opens requests with

## Related

- [moq-net traits](/quest/m2/net-wrap/traits.md) - the request traits generic handlers serve through
