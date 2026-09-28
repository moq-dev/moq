# [XS] The opening-snapshot test waits for real enrollment

## Goal

`archive::tests::an_opening_snapshot_records_every_rendition` in
`rs/moq-cli/src/archive.rs` passes under load. It fails with "writer closed"
because it treats a rendition's `.info` file as proof the export enrolled
that track, then finishes the tracks and the catalog; under load the writer
can still be subscribing when the tracks end. Seen while landing
[#4169](https://github.com/moq-dev/moq/pull/4169).

## Plan

- Wait on the signal enrollment actually produces (the subscription reaching
  the publisher, or an explicit event from the exporter), not a file that
  appears earlier. No longer timeout and no retry.
- If the exporter can genuinely lose a rendition that ends right after it
  appears in the catalog, that is a bug in the exporter, not the test; fix it
  there.

Public API: none. Wire: none.

## Related

- [More tests hold up under load](/quest/m1/test-flakes-2.md) - the same
  round of load-only failures on `main`
