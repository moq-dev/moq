# [S] Default SEI separation implementation

## Goal

Importers targeting the versioned separated-SEI profile strip H.264/H.265 SEI
into the top-level `sei` section by default. The legacy profile retains
in-band SEI, no profile duplicates metadata, and video never waits for SEI.

## Plan

Remove the temporary importer opt-in for the separated profile and update its
API, fixtures, and changelog. Selecting the new profile for deployed
broadcasts and migrating existing content stay moq.pro (downstream) work, out
of scope here.

Run the fixtures with on-time, missing, lost, and delayed sidecars. Prove the
legacy profile remains byte-faithful and the separated profile never delays
video or emits an in-band duplicate.

## Required

- [Rust SEI split and stitch](/quest/m2/sei/sei-rust.md) - supplies the opt-in
  importer behavior whose default changes here
