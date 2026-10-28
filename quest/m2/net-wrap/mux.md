# [L] moq-mux takes the traits

## Goal

moq-mux writes and reads through the
[moq-net traits](/quest/m2/net-wrap/traits.md), defaulting to moq-net's
types: the importers, catalog, container producers and consumers, timeline,
json tracks, `Source`, and TS export. The moq-video and moq-audio
encode producers built on moq-mux take them too. Handed an encrypted
broadcast, every importer and capture encoder publishes it, and every
exporter reads one, without knowing.

## Plan

- Entry points include `container::Producer::new(track::Producer)`, the
  importers' `broadcast::Producer` (`create_track` call sites throughout
  `import/`), `catalog/`, `timeline.rs`, `json.rs`,
  `source.rs`, and the TS exporter under `container/ts`. About 250 type
  references across these modules.
- The [m1 compression migration](/quest/m1/compress/README.md) deleted
  `moq-mux::binary`; opaque bytes use plain tracks, so there is no binary
  helper to convert here.
- Capture: `moq_video::encode::Producer::new` takes a concrete
  `moq_net::broadcast::Producer` and `moq_audio::encode::Producer::new` a
  `&mut` one (`rs/moq-video/src/encode/producer.rs`,
  `rs/moq-audio/src/encode/producer.rs`), so `moq import capture` could not
  hand them an encrypted broadcast. Make both generic the same way.
- Demand gating stays as it is: importers and `Source` read
  `track::Demand` and preferences through the trait.
- No behavior change with the defaults. Keep the `containers` Criterion
  bench flat before and after.
- Test an import and export round trip (fMP4 and TS) through the moq-net
  pass-through test wrapper, plus one through moq-e2ee's wrappers once
  [they implement the traits](/quest/m2/net-wrap/e2ee.md).

Public API: generic type parameters with moq-net defaults across moq-mux.
Wire: none.

## Required

- [moq-json, moq-stats, and hang take the traits](/quest/m2/net-wrap/json.md) - moq-mux writes catalogs and json tracks through them
