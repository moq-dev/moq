# [S] Publishers learn their bitrate cap

## Goal

An honest client targets its cap instead of discovering it through
backpressure: the in-band AUTH grant carries the session's `publish.rate`
and `subscribe.rate`, and `@moq/publish` (and the Rust publisher) clamp the
encoder's target bitrate to `publish.rate`.

## Plan

The public grant today carries publish patterns, subscribe patterns, and an
expiry; add the two rates in the same nested shape as the
[claim](/doc/bin/relay/auth.md#claims). The encoder clamp leaves headroom for audio
and container overhead under the cap. A grant change (refresh, union)
re-clamps a live encoder.

Decided in the quest audit: this quest owns net's in-band grants, their lite
codecs, relay propagation, and publisher clamps. Token and HTTP grant types in
Token claims carry no bitrate caps since [auth access is patterns](/quest/m1/api-audit/auth-access.md) removed the field nothing enforced; whichever rate quest enforces first re-adds an optional `rate` to the claims with a rate type moq-auth owns (converted at the relay boundary, so moq-auth never depends on moq-net), and this quest reuses the admitted caps.

Update the AUTH grant in `drafts/draft-lcurley-moq-lite.md` and run
`just drafts check`. Rates target the next unpublished lite version after finalized lite-07,
not lite-07 or its old wip identifier. Leave the
negotiated IETF AUTH extension and `drafts/draft-lcurley-moq-auth.md`
unchanged: extending cap reporting to IETF is outside this quest. Keeping
that boundary avoids expanding the agreed lite-only wire change.

Tests: a capped session's grant carries the rates; the publisher's video
target never exceeds the cap minus overhead; a raised cap lets it climb back.

Public API: the in-band grant types in `rs/moq-net` and `js/net` gain the two
rates, and the publishers gain a clamp. Wire: the AUTH grant gains optional
rates, on the next unpublished lite version after lite-07 only.

## Required

- [In-band auth](/quest/m1/auth/README.md) - the AUTH grant this extends
