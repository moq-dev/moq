# [M] moq-json, moq-stats, and hang take the traits

## Goal

moq-json's snapshot, stream, and window producers and consumers, the
moq-stats producer and consumer, and hang's catalog, container, stats, echo,
and timeline tracks are generic over the
[moq-net traits](/quest/m2/net-wrap/traits.md), defaulting to moq-net's
types, so each publishes into and reads from a wrapped track unchanged.

## Plan

- moq-json takes the trait, not a wrapper role (decided 2026-10-20,
  maintainer): its `Encoder`/`Decoder` codec layer stays as it is, and only
  `Producer::new(track::Producer)` and `Consumer::new(track::Subscriber)` in
  `snapshot`, `stream`, and `window` become generic.
- hang: `catalog::Consumer::new(track::Subscriber)`,
  `Root::subscribe(&broadcast::Consumer)`, and
  `container::Frame::write_to(&mut group::Producer)` are the entry points.
- moq-stats: the produce and consume paths that hold moq-net broadcasts and
  groups.
- Group rolls these crates trigger themselves keep working through a
  wrapper, as do demand-gated encoders.
- Existing tests keep passing with the defaults. Add one round trip per crate
  through moq-net's shared pass-through test wrapper, and a moq-json snapshot
  round trip through an encrypted track in moq-e2ee's tests.

Public API: generic type parameters with moq-net defaults on moq-json,
moq-stats, and hang. Wire: none.

## Required

- [moq-net traits](/quest/m2/net-wrap/traits.md) - the traits these take
- [moq-e2ee implements the traits](/quest/m2/net-wrap/e2ee.md) - the encrypted track the snapshot round trip runs through
