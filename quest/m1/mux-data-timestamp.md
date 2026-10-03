# [S] moq-mux data producers take a broadcast-clock timestamp

## Goal

A moq-mux JSON or binary data producer accepts a timestamp on the broadcast
clock and publishes it as given, with no ahead-of-now refusal. That timestamp
may be the source's own timestamp on a catalog clock anchored to that source
(KLV next to video from one MPEG-TS program), or the `at` of a consumer's
`Timed` the value was derived from (MAVLink telemetry republished). Callers that hold a capture
`Instant` convert it with a public `Clock::capture`.

Out of scope: what `at: None` means, which
[Publishing never invents a timestamp](/quest/m1/publish-timestamp.md)
changes, along with moq-json, moq-flate, moq-ffi and the
bindings.

## Plan

Requested by OneTooMany (Discord), who align KLV and MAVLink-derived
telemetry with video. Split out of `publish-timestamp` (2026-10-02) because
it needs neither untimed frames nor the FFI namespace move, so it can land
well before the rest.

Decided (2026-10-02):

- `Snapshot::update` / `Stream::append` in `json` and `binary` take
  `Timed<_, Timestamp>` instead of `Timed<_, Instant>`. That's the same
  contract as `container::Frame`, and the default `Timed` that moq-json and
  moq-flate already take. `at: None` keeps stamping the broadcast clock's now
  until `publish-timestamp` makes it untimed.
- `Clock::capture(Instant)` becomes public. It keeps refusing an instant
  ahead of now or before the epoch (`Error::InvalidCapture`).
- Carried over from `publish-timestamp` (2026-10-01): a timestamp ahead of
  now is recorded as zero delay rather than refused. A source clock running
  slightly fast must not be rejected.

Things to look out for:

- An ahead-of-now sample can drag the broadcast-wide delay minimum down and
  inflate delay or jitter on other tracks. Decide "ahead of now" against the
  catalog clock's current reading. Don't clamp the estimator's signed
  lateness itself: it is measured from an arbitrary local epoch, where valid
  samples can be negative. Test it.
- Callers inside the repository (moq-ffi, moq-c) pass bare values and
  should compile unchanged. The changes are moq-mux's own tests and doc
  examples that use `.at(Instant)`.
- Document on the producers that a carried-over timestamp lines up with
  media only on a broadcast sharing the source's clock mapping. Update
  `doc/lib/rs/moq-mux.md`, which describes the `Instant` input and the
  refusal.

Test: a source timestamp on a catalog clock anchored to that source is
published unchanged. A timestamp ahead of now is accepted, and other tracks' delay
doesn't move.

Public API: breaking. Wire: none.

## Related

- [Publishing never invents a timestamp](/quest/m1/publish-timestamp.md) - changes what `at: None` means for these producers, after this lands
