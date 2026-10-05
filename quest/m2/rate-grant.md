# [S] Publishers learn their bitrate cap

## Goal

An honest client targets its cap instead of discovering it through
backpressure: the in-band AUTH grant carries the session's `publish.rate`
and `subscribe.rate`, and `@moq/publish` (and the Rust publisher) clamp the
encoder's target bitrate to `publish.rate`.

## Plan

The public grant today carries publish patterns, subscribe patterns, and an
expiry; add the two rates in the same nested shape as the
[claim](/quest/m2/rate-claim.md). The encoder clamp leaves headroom for audio
and container overhead under the cap. A grant change (refresh, union)
re-clamps a live encoder.

Tests: a capped session's grant carries the rates; the publisher's video
target never exceeds the cap minus overhead; a raised cap lets it climb back.

## Required

- [In-band auth](/quest/m1/auth/README.md) - the AUTH grant this extends
- [Bitrate claim](/quest/m2/rate-claim.md) - the shape it reuses
