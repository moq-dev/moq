# [L] moq-mux takes the traits

## Goal

moq-mux writes and reads through the
[moq-net traits](/quest/m2/net-wrap/traits.md), defaulting to moq-net's
types: the importers, catalog, container producers and consumers, timeline,
json and binary tracks, `Source`, and TS export. Handed an encrypted
broadcast, every importer publishes it, and every exporter reads one,
without knowing.

## Plan

- Entry points include `container::Producer::new(track::Producer)`, the
  importers' `broadcast::Producer` (`create_track` call sites throughout
  `import/`), `catalog/`, `timeline.rs`, `json.rs`, `binary.rs`,
  `source.rs`, and the TS exporter under `container/ts`. About 250 type
  references across these modules.
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
