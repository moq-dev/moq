# [M] The catalog types carry moq-net's types, once

## Goal

`hang::Catalog` is the one struct that lists the catalog's sections, its
clock is a `Timestamp`, its archive is one record, and every section is
read through the same `Section` trait. No consumer unpacks a timestamp into
`(u64, u32)` or derefs a plain struct to reach a field.

## Plan

- `hang::Catalog<E = ()>` gains `#[serde(flatten)] pub ext: E` (the way
  `timeline::Record<E>` already works) and `moq_mux::catalog::hang::Catalog`
  is deleted. That copy re-lists every base field with the same serde
  attributes and grew from two fields to seven on dev; every new section is
  a three-place edit today, the third being `js/hang/src/catalog/root.ts`.
- `catalog::Clock { wall: Timestamp }` with serde emitting `{ wall,
  timescale }`; `Clock::new(wall)` refuses `> 2^53 - 1` and a zero scale;
  `wall_clock(&self, pts: Timestamp)`. Delete `with_timescale`,
  `default_timescale`, and `deserialize_wall` from the surface. Both real
  callers (`rs/moq-mux/src/clock.rs`, `rs/moq-hls/src/export/rendition.rs`)
  hold a `Timestamp` and unpack it.
- Fold `catalog::Timeline` into `Archive` and drop the `Deref`/`DerefMut`/
  `From<Timeline>`; after the dead per-track field goes it appears nowhere
  else. `moq_mux::timeline::Consumer::subscribe` takes `&Archive`. JS
  `ArchiveSchema` stops extending `TimelineSchema`.
- `impl Section for Text { const MAP = "renditions"; }` and delete
  `deserialize_text`.
- `moq_mux::Clock`: `new()` and `at(epoch, wall)` replace three
  constructors; `now() -> Timestamp` replaces `micros() -> u64`; `wall()`
  returns the catalog `Clock`.
- `catalog::Producer::new(broadcast, Config)` is the one constructor;
  `with_catalog` duplicates `Config::with_catalog` and `with_timeline`
  (no caller) re-takes the broadcast and silently replaces an enrolled
  timeline.
- JS parity in the same pass: the archive `timescale` bound matches Rust's
  u32; the timeline `durationMin`/`durationMax` props are `Time.Milli`;
  `Catalog.TRACK` and `Catalog::DEFAULT_NAME` agree on a name; the JS
  timeline moves from `Container.Timeline` (it is not a container) to a
  `Hang.Timeline` namespace, matching the Rust split between `hang::timeline`
  records and `moq_mux::timeline` machinery.

Public API: breaking on hang, @moq/hang, moq-mux, and moq-hls, so on dev.
Wire: none (serde shapes are unchanged). Consumers: moq-mux, moq-hls,
moq-cli, moq.pro's overlay and recorder.

## Related

- [Rendition ownership](/quest/m1/api-mux-rendition.md) - the producer-side reshape on the same crate
- [Catalog consumer](/quest/m2/hang-catalog-consumer.md) - the reader both languages lack
