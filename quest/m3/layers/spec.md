# [M] hang specifies a composition

## Goal

`drafts/draft-lcurley-moq-hang.md`, `rs/hang`, and `js/hang` define a
composition: a versioned catalog section that lists child broadcasts, marks
one as main, and gives each child its offset from its PTS onto the
composition's clock, its priority, whether a viewer may toggle it, and, for
sound, its gain, mute, pan, and delay. A separate list of pictures places
children: each picture names a child and carries its transform, crop,
z-order, opacity, visibility, and keyframes, so one child can show in
several places. The
composition's catalog also lists the main child's renditions, so a consumer
that knows no compositions plays the main alone.

## Plan

The [epic](/quest/m3/layers/README.md) holds the 2026-11-12 decisions: the
composition lives in its own broadcast's catalog, applied at each update's
timestamp; offsets are the composer's; a composition needs a timed catalog,
and a consumer plays the main alone in a scale-less one.

Start from the format moq-dev/studio's prototype settles, and keep only what
every player can reproduce. A consumer refuses an unknown version loudly and
plays the main alone. Children name broadcasts, relative like `include`'s and
contained the same way (`check_resolvable` in
`rs/hang/src/catalog/consumer.rs`), never files. Unlike an include, a child
keeps its own PTS timeline and root settings, and listing one does not
subscribe to it. The composition's clock is the main's, so the main's offset
is zero and the catalog lists its renditions by their `broadcast` reference,
on the same timeline.

Decided 2026-11-12 (maintainer, moq-dev/studio /quest-complete on
[moq-dev/studio#146](https://github.com/moq-dev/studio/pull/146)): ✅
placements are a list of pictures that each name a child, apart from the
child entries (rejected: one placement per child). A studio source can show
in several places at once, such as a clone or the top layer, and a player
fetches its broadcast once however many pictures show it.

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): a sound child
carries pan and delay as well as gain and mute. The studio's strip has all
four (`rs/moq-scene/src/collection.rs` in moq-dev/studio; pan shipped in
[moq-dev/studio#116](https://github.com/moq-dev/studio/pull/116)), and a
source's own broadcast is taken before the strip applies them. Without the
delay, a microphone the creator delayed for sync would play out of sync for
viewers.

## Waiting

- moq-dev/studio: the studio publishes a provisional composition and its
  `moq-studio --watch` prototype plays it, planned as
  `quest/m2/compose/layout.md` there. Check: that quest's file is gone from
  moq-dev/studio's `main` because its PR merged.

## Required

- [hang applies catalog updates at their timestamp](/quest/m2/catalog-at-timestamp.md) - a composition's changes land on the frame stamped alike
