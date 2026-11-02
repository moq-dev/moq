# [S] Publishers learn their bitrate cap

## Goal

An honest client targets its cap instead of discovering it through
backpressure: the in-band AUTH grant carries the session's `publish_rate`
and `subscribe_rate`, and `@moq/publish` (and the Rust publisher) clamp the
encoder's target bitrate to `publish_rate`.

## Plan

The public grant today carries publish patterns, subscribe patterns, and an
expiry; extend it with the two rates admitted by the transport policy.
The encoder clamp leaves headroom for audio and container overhead under the cap. A grant change (refresh, union)
re-clamps a live encoder.

Decided in the quest audit: this quest owns net's in-band grants, their lite
codecs, relay propagation, and publisher clamps.

Token claims and HTTP grants carry optional caps as `moq_auth::Rate`
(`publish_rate`, `subscribe_rate`, bits per second), converted at the relay
boundary so moq-auth never depends on moq-net. QUIC sessions enforce them and
every other transport refuses them (`rs/moq-relay/src/auth/caps.rs`). Its
parsers and refusal rules must prevent any transport without enforcement from
silently discarding a cap. This quest reuses only caps that admission actually
enforces.

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
