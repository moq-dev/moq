# [M] A finishing publisher lingers for max-age

## Goal

Every publisher that ends a broadcast cleanly keeps it up for the media
max-age afterwards, so a subscriber still catching up (one that read the
final catalog late, or is draining a backlog) can request and drain the
finished tracks. Cached content stays servable until it is stale.

## Plan

Decided (2026-10-11): a retention rule, not a race-hiding sleep. The bound is
the tracks' max-age, never a new constant, and an interrupt (SIGINT/SIGTERM,
or the library equivalent) ends it early.

`moq import` from stdin already does this (#5287). Extend the rule
to the other finishing publishers: the CLI's gateway imports (HLS, RTMP, SRT,
RTC), `moq-gst` sinks, the FFI and its bindings, and `@moq/publish`.

Decide where the rule lives. Recommended: the library, so a finished
`broadcast::Producer` (or the session close that follows it) stays servable
for its tracks' max-age without each publisher reimplementing it. Then
`moq import` drops its own linger. Report the public API shape before
landing it.

Test with a mocked clock: a subscriber that requests a finished track within
max-age gets its media, and the publisher closes once max-age passes.

## Related

- [One max_age meaning](/quest/m1/cache-max-age.md) - what max-age means for a superseded group
