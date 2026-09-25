# [XS] Stop writing the legacy stats counter names

## Goal

`Traffic` and `Presence` serialize only the canonical `*_started` /
`*_ended` names, dropping the `announced`, bare-plural (`broadcasts`,
`subscriptions`, `sessions`), and `*_closed` spellings written beside them
today. Deserialize keeps accepting both, so a new reader still reads an old
relay. Wire only; no API change.

## Plan

- The serializers live in `rs/moq-net/src/stats.rs` (`TrafficSer`,
  `PresenceSer`). Drop the legacy fields there and update the
  serialization tests that pin both spellings.
- The demo dashboard (`demo/web/src/stats.ts`) already prefers the
  canonical names; leave its fallback in place for older relays.
- `doc/bin/relay/config.md` (`[stats]`) and the `moq-stats` crate docs stop
  promising the old spellings.

## Required

- moq.pro reads the `*_started` / `*_ended` stats names
