# [S] The API diff reports GStreamer element properties

## Goal

`just api-diff` lists added, removed, and changed properties of `moqsink`,
`moqsrc`, and their pads (name, type, flags, default) between two refs, so
a removed property shows up in the cut's API review. Today
`cargo-semver-checks` sees only Rust items and reports moq-gst unchanged,
which is how the `encoder` pad property left with no MIGRATE entry.

## Plan

Found by the 2026-11-07 API shape audit (bindings). Dump the properties for
each ref with `gst-inspect-1.0` on a built plugin (or a small Rust test that
walks the elements' `ParamSpec`s, which avoids a GStreamer install
difference between refs) and diff the dumps in `sh/api-diff/`. The missing
`encoder` MIGRATE entry itself is in
[migrate-audit](/quest/m3/release/migrate-audit.md).
