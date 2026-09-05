# [S] moq-net: a zero-budget subscriber still receives discontinuity markers

## Goal

On `dev`, a live-edge subscriber (`max_age` budget of zero, the default)
still receives an empty group, so the codec reset it marks reaches the
decoder. Today `is_stale` sheds the empty group the moment a newer group
exists, the container consumer sees a plain sequence gap, `discontinuity()`
never bumps, and the decoder is fed the new epoch without a reset.

## Plan

A dev-only regression from the interaction of max-age shedding (#3251) with
the idle-capture markers (#3214): an empty group's reach is bounded by its
successor's start, so its age equals the successor's span and a zero budget
sheds it. With undeclared rewinds going away
([Monotonic timeline](/quest/m1/monotonic-timeline.md)), the declared marker
is the only reset signal there is, so losing it is not an option.

Exempt a finished empty group from `is_stale`: it costs nothing to deliver
and carries the reset. The alternative, having the container layer infer a
discontinuity from any sequence gap, would fire on every ordinary shed and
reset decoders that need no reset. State the exemption in the moq-lite draft's
expiration text if the wording implies every group ages alike.

Test: a producer emits group 1, an empty group, then group 2; a zero-budget
subscriber observes `[1, 0, 1]` at the transport layer and the container
consumer's `discontinuity()` bumps once. Fix on `dev`.

## Closes

- [#3291](https://github.com/moq-dev/moq/issues/3291) - close this issue when the quest finishes

## Related

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - makes the declared marker the only reset signal
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - the other open-group lifecycle rule in the same area
