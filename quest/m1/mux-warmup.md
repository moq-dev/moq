# [M] Every moq-mux consumer gets the catalog warmup from one entry point

## Goal

A moq-mux container consumer is built from a subscription delay and the
catalog rendition's `warmup` in one call. That call widens the subscription
and configures the consumer, so a caller can't forget the warmup or apply it
in the wrong order. Every raw consumer uses it: moq-audio, moq-c, moq-ffi's
`MoqMediaTrackConsumer`, and moq-gst's `moqsrc`. moq-ffi's raw consumer and
`moqsrc` then start early enough for a warmed-up decoder, as moq-c's
`moq_consume_audio`/`video` already do.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7723](https://github.com/moq-dev/moq/pull/7723)): milestone m1.
#7723's two follow-ups are one quest, since the simpler entry point is how
the two missing consumers get the warmup.

- Today (`rs/moq-mux/src/container/consumer.rs:340-371`, and
  `rs/moq-mux/MIGRATE.md:18`) the caller must add the warmup to the
  subscription's `delay.max` before subscribing. It must then call
  `with_warmup`, which carves the warmup back out, and call it before
  `with_delay`. moq-audio (`decode/consumer.rs:245`) and moq-c
  (`consume.rs:540-550`) do this by hand.
- Missing today: `rs/moq-ffi/src/media.rs:1717-1748`
  (`MoqMediaTrackConsumer::subscribe`) and
  `rs/moq-gst/src/source/imp.rs:1649-1673` subscribe with the caller's delay
  and never call `with_warmup`. The deleted moq-c quest
  (`quest/m1/c/raw-consume-warmup.md`, removed in `cd7f9a9a38`) wrongly
  assumed moq-ffi already matched.
- Shape: one moq-mux constructor or builder takes the track or subscription
  request, the `Delay`, and the warmup. It returns the widened subscription
  and a consumer configured to match, or subscribes itself. Remove the
  order-sensitive `with_warmup` + `with_delay` pair from the public API,
  and record the break in `MIGRATE.md`.
- moq-video has its own two-decoder scheme (`decode/consumer.rs:176-185`).
  Move it onto the new entry point if it fits. If not, record why here.
- Test each consumer: a test that starts a track with a nonzero catalog
  warmup fails without that consumer's adoption.
- Start after [subscription delay](/quest/m1/subscription-delay.md)
  ([moq-dev/moq#7697](https://github.com/moq-dev/moq/pull/7697)). It
  replaces the bindings' `max_delay` with `Delay { min, max }` and edits
  `rs/moq-ffi/src/media.rs`.

Public API: moq-mux's container consumer setup changes (breaking). The
bindings' subscribe surface is unchanged. Wire: none.

## Required

- [Subscriptions take a Delay](/quest/m1/subscription-delay.md) - lands the bindings' `Delay { min, max }`, which this builds on
