# [M] Encrypted broadcasts serve undeclared track names

## Goal

An encrypted broadcast producer answers requests for track names it never
created or reserved, as a plaintext `moq-net` broadcast does. Its request
handler (`requests()`) mirrors moq-net's and
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
- A refused name holds nothing: no claim, no derived key, no queued
  request. One that does not decode or has an invalid length is refused
  before any open; a forged, older-epoch, noncanonical, or non-UTF-8 name
  costs one open. Test a
  relay-forged name, an older epoch's name, and a burst of forged names
  that leaves no state behind.
- A handler that drops an idle track must be able to serve the name again
  when it is requested later, as with moq-net. moq-net already keeps one
  sequence namespace per name for the broadcast's life and refuses a reused
  sequence (`replacement_refuses_reused_sequences`). Recommended: the claim
  keeps the name's high-water marks across every broadcast of the
  generation, and a re-served name resumes above them instead of being
  refused. That keeps every `(group, frame)` identity unique without
  releasing the claim. Say in the PR whether `create_track` adopts the same
  rule, which today refuses a re-created name.
- Listing (maintainer, 2026-10-24): the wrapper's handler offers moq-net's
  `lister`, taking semantic names and listing their physical names. An
  e2ee consumer's listing decrypts them back
  ([moq-e2ee implements the traits](/quest/m2/net-wrap/e2ee.md)), so a
  listed name round-trips.
- Rust only: `@moq/net` has no public broadcast request handler yet;
  [JS undeclared names](/quest/m2/e2ee/js-undeclared-names.md) adds it and
  the `@moq/e2ee` mirror.
- Recovery is already in both cores: crate-private
  `generation::Producer::open` in Rust (drop its `allow(dead_code)` once
  the handler calls it) and `@internal` `Generation.Producer.open` in JS.

Public API: a request handler on `moq_e2ee::broadcast::Producer`, and
`track::Request::name()` returns the semantic name. Wire: none.

## Related

- [moq-net traits](/quest/m2/net-wrap/traits.md) - the request traits generic handlers serve through
