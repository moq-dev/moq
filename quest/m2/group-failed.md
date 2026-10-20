# [M] Publishers learn which groups failed to deliver

## Goal

A publisher can observe a group it produced whose delivery a subscriber's
stream gave up on (reset as `Old`, evicted, or the stream failed), with one
shape in Rust `moq-net` and in both `js/net` publishers, lite and IETF.

## Plan

Decided with the maintainer on 2026-10-20, from
[#6059](https://github.com/moq-dev/moq/pull/6059), which dropped JS
`Track.Producer.failed` because Rust had no counterpart and it covered only
the lite publisher.

- Name the consumer first, such as an encoder that forces a keyframe or a
  stats counter, and let it decide what the notification carries. If none
  exists by the time this starts, propose deleting the quest.
- Return a handle or an event stream, not a callback.
- A relay fans a track out to many subscribers, and a hostile subscriber can
  fail streams on purpose. Coalesce per group so N subscribers failing one
  group cost one event, and benchmark the path swept over subscribers and
  groups.
- Carry it through moq-ffi and the bindings only once a binding consumer
  asks.

Tests on mock time, Rust and JS: a group reset as `Old`, an evicted group, and
a failed stream each surface once, on lite and IETF.

Public API: new, mirrored across Rust and JS. Wire: none.

## Related

- [An evicted group is judged stale](/quest/m1/evicted-group-stale.md) - more stalled groups reset as `Old`, which this would surface
