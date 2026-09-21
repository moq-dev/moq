# [M] Archive listing has one recording-scoped query

## Goal

Streaming and paginated archive listings use the same prefix and offset
convention and expose only behavior their result type can represent. Callers
cannot accidentally prepend a recording prefix twice or request directory
results that are silently discarded.

## Plan

Today `List::prefix` takes a fully prefixed Path, while `list_paginated`
takes a relative string. The latter exposes object_store's entire
PaginatedListOptions but drops common_prefixes from its result. Its bare
recording prefix can also match a sibling such as `rec-other` when listing
`rec`, because paginated backend prefixes are lexical rather than segmented.

Use an archive-owned `store::list::Query` and `Entry`, with one
recording-relative prefix/offset convention across both entry points. Keep
continuation tokens opaque and scope them to the query. Expose pagination
controls the archive supports, not arbitrary backend options. Directory
listing is outside this flat object API. Keep the generic Store and its
object_store escape hatch; do not add another storage abstraction.

Fold duplicate free path-prefix helpers into the Store/query API and make
implementation-only helpers, including check_id, crate-private. Keep module
docs and examples consistent and report the exact removed exports.

Regression tests cover a nonempty recording prefix, sibling recordings,
track prefixes, exclusive offsets, multiple pages, and every exposed option.
Passing the same logical query to either method must enumerate the same
objects when collected; backend order remains unspecified. A page must
neither silently drop directory results nor fail on neighbouring recordings.

Public API: breaking list query and pagination options in moq-archive 0.0.1.
Wire and persisted format: unchanged.

## Related

- [Archive proof](/quest/m2/archive/proof.md) - backend and replay conformance
