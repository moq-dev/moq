# Composed broadcasts

## Goal

hang specifies a composition: a broadcast whose catalog lists other
broadcasts, each on its own timeline with an offset onto the composition's
clock, and says where and when each shows and how its sound mixes. `moq play`
and `@moq/watch` play a composition, so a moq-dev/studio broadcast plays as
its creator composed it, while each source stays its own broadcast that a
viewer can toggle and fetch at its own priority.

This README's own work is the end-to-end proof: one fixture composition with
a main broadcast, a child shown in a picture, and a child heard as sound plays in `moq play` and
`@moq/watch` with the same placement and timing, within a stated tolerance,
and with the child's sound mixed in.

## Plan

Replanned 2026-11-12 (maintainer, moq-dev/studio /quest-plan), after the
studio chose a composition over a layer layout track for naming guests (its
`quest/m2/compose/layout.md`). hang's `include` requires an included
broadcast to share the listing catalog's PTS timeline, `media.clock`, and
`media.join.interval`, which a guest's broadcast does not.

- **What:** ✅ a composition (rejected: a layer layout track naming tracks of
  one broadcast, the 2026-11-08 plan). Every independently published source
  is its own broadcast, and the composition is a broadcast of its own that
  lists them; the studio's program is one child.
- **Generality:** ✅ a general way to compose broadcasts, such as multiview or
  picture-in-picture, keeping only what every player can reproduce
  (rejected: exactly what the studio emits). The studio is its first
  producer.
- **Shape:** ✅ all in the composition broadcast's catalog, its updates timed
  (rejected: the catalog listing children with a separate timed track for
  placement; everything in one track). A catalog update applies at its
  timestamp ([that rule](/quest/m2/catalog-at-timestamp.md), m2).
- **Clock:** ✅ each child carries the composer's offset from its PTS to the
  composition's (rejected: aligning children by their `media.clock` wall
  times, which depends on publishers' clocks agreeing and drops the latency
  the composer saw). A child that restarts gets a new offset in a new update.
- **Older players:** ✅ the composition names one child as main, and its
  catalog lists the main's renditions, so a player that knows no
  compositions plays the main picture alone (rejected: an empty catalog for
  them).
- **Untimed:** ✅ a composition requires a timed catalog; in a scale-less one
  it is a publisher error, and a player plays the main alone (rejected:
  applying updates on arrival).

Still settled from 2026-11-08: hang specifies the format and each player
implements it with its own code, so parity comes from the shared fixture, not
shared code (moq never depends on moq-dev/studio); the studio prototypes the
format first. Players mix each child's sound at the gain, mute, pan, and
delay the composition carries. Each child carries a priority and whether a
viewer may toggle it; a player maps priority onto moq-lite's subscription
priorities and unsubscribes a toggled-off child. m3: moq-dev/studio is the
named consumer, and its side sits in its m2.
