# Congestion-aware transcode ladders

## Goal

A transcode ladder respects the uplink it publishes over. `moq-transcode`
opens one encoder per demanded rung at its configured maximum and publishes
every rung across one first-mile connection, with no bandwidth input and no
live rate control, so congestion leaves every active rung producing at its
ceiling.

This is first-mile adaptation, not per-viewer adaptation. Sharing one output
broadcast across several independently constrained output sessions stays out
of the model: one encoder and one catalog bit cannot represent per-session
state.

## Plan

The catalog and player half already shipped in
[moq#2865](https://github.com/moq-dev/moq/pull/2865): the optional `stalled`
state exists in `rs/hang`, `js/hang`, `rs/moq-msf`, `js/msf`, the HANG draft
and `moq_consume_video_stalled`; `@moq/watch` filters stalled renditions with
the all-stalled lowest fallback; and routing, decoder, and presentation
identities are split so a metadata-only change cannot rebuild WebCodecs.

What remains is the publisher side. The allocator on `dev`
([moq#2854](https://github.com/moq-dev/moq/pull/2854)) already divides a
connection's estimate by `track::Info::priority`, filling a tier before the
next sees a bit and splitting max-min fair within one. So a controller that
assigns descending priorities down the ladder gets correct allocation with no
scheduler change. Transmission order is the separate question, and the
allocator's own docs say so: it "is not what orders the local send queue: that
ranks by each subscription's own priority".

### Adaptive bands

`VideoConfig.bitrate` stays the configured maximum and never follows the
instantaneous target. For a rendition with configured maximum `max` and the
next lower rendition's `lower`:

```text
stall = (max + 2 * lower) / 3
```

The lowest rendition takes `lower = 0`, so its boundary is `max / 3`. An
encoder may adapt within `[stall, max]`; at the boundary it clamps and
publishes `stalled: true`, cleared only once a target above the same boundary
is successfully applied. Catalog state follows the last target the encoder
*accepted*, not the one the controller requested, so a transient rate-control
failure retains the last applied target rather than lying.

Start with the existing rate controller's five percent hysteresis, immediate
decreases, and gradual upward ramp. No second re-entry threshold and no dwell
timer until measurements show the catalog state flaps.

### Non-goals

Per-viewer or per-session rendition state; rewriting the advertised maximum
bitrate; closing or removing stalled tracks; an application-level
`unselectable` state; stall counters in the catalog (those are telemetry,
[#2734](https://github.com/moq-dev/moq/issues/2734)); rebuilding unsupported
encoders on every target change.

## Quests

- [Controller](/quest/m1/ladder/controller.md) - one controller owns every
  rung's share, target, and stalled state
- [Fetch and catalog](/quest/m1/ladder/fetch.md) - uncached FETCH encodes at
  the shared applied target, and rung state survives a source catalog refresh
- [Transmission order](/quest/m1/ladder/transmit.md) - lower renditions win
  the publisher-side tie-break among equal subscriber priorities

## Closes

- [#2858](https://github.com/moq-dev/moq/issues/2858) - close this issue when the questline finishes

## Related

- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the other sender that reserves but never follows its grant
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - passthrough tracks that reserve nothing at all
- [#2857](/quest/m1/plan-binding-rate-control.md) - non-Rust publishers cannot reach rate control regardless of what the ladder does
