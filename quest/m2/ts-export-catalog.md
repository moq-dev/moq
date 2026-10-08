# [S] ts::Export takes a catalog stream

## Goal

`ts::Export::new` takes `(source, catalog)` like `flv::Export`, `fmp4::Export`,
and `mkv::Export` (#4987), and `with_catalog_format` moves onto the catalog
stream, with no new public type and no compatibility shim.
