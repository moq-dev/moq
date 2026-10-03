# [S] Plan: rename max-age to max-delay

## Goal

Decide whether the staleness budget is renamed from `max_age` to `max_delay`
everywhere: the CLI flags (publish and import retention, rtmp, subscribe), the
moq-net `Subscription::max_age` and `track::Info::max_age`, js/net's `maxAge`,
and the bindings. Then rewrite this quest into implementation quests, or
delete it.

## Plan

The maintainer finds "delay" clearer than "age" for how far a group may fall
behind before it's skipped. `moq play --delay` and `moq export ts --delay`
(planned in [fixed-delay release](/quest/m1/tstd/delay.md)) use one delay
knob for presentation and staleness; only `moq play`'s exists today.

To weigh:

- Publisher retention (`track::Info::max_age`) really is an age of cached
  content, while the subscriber budget is a delay. They may deserve different
  names rather than one rename.
- The wire fields are `Publisher Max Age` and `Subscriber Max Age` in
  `drafts/draft-lcurley-moq-lite.md`.
  Renaming the draft's field is free on the wire, but it churns the spec.
- A rename breaks every published API and binding, so it lands
  mirrored across Rust, JS, and the bindings in one release.
