# [M] Max-delay decoders join a group earlier even when the live edge arrives first

## Goal

On a cold network join, the max-delay decoder of a `warmup` rendition starts
at the group before the live edge, in both `moq_video::decode::Consumer` and
the browser [dual decoder](/quest/m2/intra-refresh/dual-decoder.md), even
when the live-edge group's stream arrives first. Every other container
consumer keeps starting where the publisher does, exactly as today.

## Plan

Decided while iterating on the native dual decoder (#5842), with the
maintainer away; the agent took the recommended option for each:

- The problem: an unfloored `moq_mux::container::Consumer` adopts the lowest
  group that has arrived when it first reads, and drops a lower group that
  arrives later as old. The upstream request aggregates to
  `Order::Descending`, so the live-edge group often wins the race. The
  max-delay decoder then starts mid-sweep, like the min-delay one, and shows
  stripes for a sweep. Cached joins are fine today: `decode::Consumer` moves
  that decoder's local cursor one group below `track.latest()`.
- An opt-in on the container consumer, not a new default. Waiting for an
  earlier group in every unfloored consumer would break
  `startup_mid_stream_live_track_starts_at_the_served_group` and delay the
  first frame of every budgeted consumer when no earlier group exists.
- Semantics: once the first group arrives, wait for the sequence below it as
  long as `Subscription::max_delay` says it could still be fresh, measured on
  both clocks the subscription documents. In media time, that lasts until
  the newest content is a budget past where the first group starts. In wall
  time, until the first group arrived a budget ago. The wall clock bounds a
  quiet or paused track, which never ages out in media time.
- One quest for both sides so the semantics match: the Rust opt-in in
  `moq_mux::container::Consumer`, used by `decode::Consumer`'s max-delay
  decoder, and the same opt-in in `js/hang`'s `Container.Consumer`, used by
  the browser dual decoder. Mirror the name across Rust and JS. `moq play`'s
  max-delay decoder (`rs/moq-cli/src/play/media.rs`) subscribes the same way
  and takes the opt-in too.
- Open, the maintainer's call: the opt-in's name and shape. Options are a
  builder such as `with_lookback(groups)`, a count of earlier groups to wait
  for; a bool such as `with_earlier_group()`; or a config field. Recommend
  the builder with a count, since a `warmup` longer than one group would
  want more than one.
- Tests, with mocked time: newest-first arrival delivers the earlier group
  first in Rust and JS; a quiet track with no earlier group starts within
  one budget of wall time; consumers without the opt-in are unchanged; a
  `decode::Consumer` cold join whose live edge arrives first hands over to a
  max-delay decoder that started a group earlier.
