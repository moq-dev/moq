# [M] Our moq-transport publishers refuse SUBSCRIBE_TRACKS

## Goal

A peer's SUBSCRIBE_TRACKS is refused with NOT_SUPPORTED by the Rust and JS
moq-transport publishers, as before
[moq-dev/moq#6167](https://github.com/moq-dev/moq/pull/6167), and the session
stays up. Rust keeps sending SUBSCRIBE_TRACKS upstream
([moq-dev/moq#6280](https://github.com/moq-dev/moq/pull/6280)), so a TRACKS
listing, and the archive that uses it, still learns an IETF peer's tracks.

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "what's not worth
supporting in IETF").

Decision prompt: ✅ cut inbound only; cut both; keep both.

Why: answering takes `rs/moq-net/src/ietf/publisher/tracks.rs` (2,558
lines) and its JS mirror in `js/net/src/ietf/publisher.ts`. That covers a
paused PUBLISH per track, FORWARD toggles, resume through REQUEST_UPDATE,
PUBLISH_SKIPPED, PREFIX_OVERLAP, and credit-gated opens, which took
[moq-dev/moq#6641](https://github.com/moq-dev/moq/pull/6641),
[moq-dev/moq#6663](https://github.com/moq-dev/moq/pull/6663), and
[moq-dev/moq#6577](https://github.com/moq-dev/moq/pull/6577). No known third
party sends it. The interop runner's SUBSCRIBE_TRACKS cases are already
listed as unsupported in
[the interop audit](/quest/m2/audit-interop.md), and our own peers list over
moq-lite. The outbound half stays because the
[archive](/quest/m1/archive/README.md) finds an IETF peer's tracks only
through it, having no catalog.

Delete `publisher/tracks.rs`, the JS answerer, their tests, and the
`subscribe-tracks` JS bench, plus any of `rs/moq-net/benches/tracks.rs`
that measures only the IETF answerer. Keep the decoder, so a refusal still
admits the Request ID first. Change `test/interop/tracks.ts` to assert the
refusal where it drove our publisher, and keep any case that drives Rust's
outbound listing against a third party. Update the `SUBSCRIBE_TRACKS`
bullets of `doc/concept/standard.md`. Add a planned break if a compat cell
covers it.

Public API: none. Wire: our publishers refuse SUBSCRIBE_TRACKS with
NOT_SUPPORTED.

## Related

- [Our moq-transport publishers serve at most one group](/quest/m1/ietf-publisher-one-group.md) - the same publisher-side narrowing for FETCH
