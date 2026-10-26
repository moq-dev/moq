# [M] moq-transport range updates

## Goal

A relay carries a downstream range change to its moq-transport upstream in the
subscription update that already carries the aggregate priority, widening
included, and resolves a `Time(pts)` end before it goes upstream.

## Plan

Range fills already go upstream as one FETCH per run of missing groups, and
our publisher serves multi-group FETCHes ending at the Largest Object's group.
That group is never cut at the Largest Object (maintainer, 2026-10-25,
departing from draft-22 section 9.11): the cache cannot know an open group's
real Largest Object, so the answer waits for it to finish rather than handing a
downstream cache a truncated group as complete. What remains is the
subscription side.

Upstream range changes ride `write_update` in `ietf/subscriber.rs`.
Narrowing is allowed from draft-15 on. Widening is a bug today: on draft-22
and later, a subscriber from a future group puts its absolute start in the
SUBSCRIBE filter, and a live subscriber merged in after it never gets the
current group. Draft-22 lets an update lower the start with FILL_PARAMETERS
for what it missed, which opens a second fill stream on the subscription.
Draft 14 forbids widening, and 15 to 18 have no fill.
`rs/moq-net/tests/integration/subscribe_ranges.rs` leaves draft-22 out for
that reason; add it back once the update carries the widened filter. Our
publisher still resets a subscription fill spanning several groups
(`FillServe::Unsupported` in `ietf/publisher.rs`); serve it from the cache as
a multi-group FETCH is served.

moq-transport has no time filter, so a downstream `Time(pts)` end is
resolved against the relay's cache before it goes upstream as a group range,
or refused when the cache cannot resolve it.

A relay's fetch-only copy refuses a draft-22 FETCH relative to the Largest
Object with Unsupported, since it does not store the Largest that
TRACK_STATUS reports. Record it on the copy so those filters resolve there
too.

## Related

- [Pipelined first FETCH](/quest/m2/pipeline-requests/fetch.md) - sends TRACK_STATUS alongside the FETCH in adjacent code (`ietf/subscriber.rs`, `model/origin.rs`); an independent m2 optimization that adapts to these ranges and does not gate them
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - handles native timeout gaps; do not mistake them for never-existing objects or media-age limits
