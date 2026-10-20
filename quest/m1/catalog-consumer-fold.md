# [S] One hang catalog consumer

## Goal

`moq-mux` reads hang catalogs through `hang`'s catalog consumer instead of
its own near-copy, so rendition and resolvability checks live in one place.

## Plan

`rs/moq-mux/src/catalog/hang/consumer.rs` and
`rs/hang/src/catalog/consumer.rs` both wrap
`moq_json::snapshot::Consumer<Catalog<E>>` with the same `new`,
`compressed`, `poll_next` (rendition and resolvability checks) and `next`.
The `moq-mux` copy adds only a `CatalogFormat` (hang or hangz) and the
`CatalogExt` bound. Fold that into `hang`'s consumer, or keep a thin
`moq-mux` adapter over it; pick whichever keeps `hang`'s public surface
smaller. Found by #5806.

Public API: `moq-mux`'s hang catalog consumer may become a re-export or
adapter. Wire: none.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - `moq-mux` catalog checks
- [Compressed tracks](/quest/m1/compress/README.md) - drops `catalog.json.z`, so the folded consumer need not keep the hangz format
