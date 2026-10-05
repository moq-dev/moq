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

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer for the
publisher-side ladder.

The catalog and player half is the rendition `enabled` flag
([enabled flag](/quest/m1/catalog-enabled.md)), which replaces the `stalled`
state shipped in [moq#2865](https://github.com/moq-dev/moq/pull/2865):
`enabled: false` means no frames are coming and a viewer must not select the
rendition. Routing, decoder, and presentation identities are split so a
metadata-only change cannot rebuild WebCodecs.

Decided (2026-10-04): a rung's `enabled` follows its last applied target. It
is disabled, with encoding stopped, when its share falls below its boundary,
and enabled once a target at or above the boundary is applied. Demand loss
alone never changes the flag.

What remains is the publisher side. The allocator
([moq#2854](https://github.com/moq-dev/moq/pull/2854)) divides a connection's
estimate by `track::Info::priority`, filling a tier before the next sees a
bit and splitting max-min fair within one. A controller that assigns
descending priorities down the ladder gets correct allocation from that
alone. Send order is a different number today: `Priority::cmp`
(`rs/moq-net/src/lite/priority.rs`) ranks first by `Priority.track`, which is
the subscriber's priority from SUBSCRIBE (`msg.priority` in
`rs/moq-net/src/lite/publisher.rs`), not the publisher's `Info::priority`.
The controller quest makes the publisher's number the tiebreak after it, so
the same number decides what to produce and, among equal subscriber
priorities, what to send first;
[Scope track priority](/quest/m1/track-priority-scope.md) settles what that
ranking means on the first mile versus a cluster session before the
controller depends on it.


### Adaptive bands

`moq_transcode::Rung::bitrate` (`rs/moq-transcode/src/ladder.rs:14-17`) stays
the configured maximum and never follows the instantaneous target. For a
rendition with configured maximum `max` and the next lower rendition's
`lower`:

```text
stall = (max + 2 * lower) / 3
```

The lowest rendition takes `lower = 0`, so its boundary is `max / 3`. An
encoder may adapt within `[stall, max]`; below the boundary the rung is
disabled (`enabled: false`) and stops encoding, and it is enabled again only
once a target at or above the same boundary is successfully applied. Catalog state follows the last target the encoder
*accepted*, not the one the controller requested, so a transient rate-control
failure retains the last applied target rather than lying.

Start with the shared rate policy's five percent hysteresis, immediate
decreases, and gradual upward ramp (`moq_mux::rate::Control`).
No second re-entry threshold and no dwell timer until measurements show the
catalog state flaps.

### Non-goals

Per-viewer or per-session rendition state; rewriting the advertised maximum
bitrate; removing a disabled rung from the catalog; an application-level
`unselectable` state; stall counters in the catalog (those are telemetry,
[#2734](https://github.com/moq-dev/moq/issues/2734)); rebuilding unsupported
encoders on every target change.

## Required

- [Controller](/quest/m3/ladder/controller.md) - one controller owns every
  rung's share, target, enabled state, and send order
- [Fetch and catalog](/quest/m3/ladder/fetch.md) - uncached FETCH encodes at
  the shared applied target, and rung state survives a source catalog refresh

## Closes

- [#2858](https://github.com/moq-dev/moq/issues/2858) - close this issue when the questline finishes

## Related

- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the other sender that reserves but never follows its grant
