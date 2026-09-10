# [M] Archive store

## Goal

`moq-archive` stores the versioned track objects of the
[Recording section](/drafts/draft-lcurley-moq-hang.md#recording) on memory, local disk, and
S3-compatible storage through a generic `T: object_store::ObjectStore`.

## Plan

Add `object_store` to `[workspace.dependencies]`; it is absent from
`Cargo.lock` today. Use it directly rather than adding a second store trait or
per-backend implementations. Callers that need runtime dispatch supply
`Arc<dyn ObjectStore>`; the archive API itself stays generic.

The layout is the format's:

```text
<prefix>/<encoded-track>/.info
<prefix>/<encoded-track>/groups/<largest>.<smallest>
<prefix>/<encoded-timeline-track>/segments/<segment>
```

Implement the percent encoding of track names here; nothing in `rs/hang/src`
does it yet. `.info` is a versioned JSON body with the immutable priority and
timescale. A segment object is the versioned binary envelope: a group/frame
table with timestamps and payload offsets, then the original payload bytes.
Encode the first group ID absolutely, then `current - previous - 1` so zero
means consecutive. Require nonempty objects, ascending IDs, checked arithmetic,
and, for range-named objects, agreement between the table and filename bounds. Bounds-check every
table entry while decoding and refuse an unknown version. Restrict `.info`
timescale to the JSON safe-integer range.

Limit recorded group and segment IDs, including reconstructed deltas, to
0 through 9007199254740991 (2^53 - 1). Refuse larger IDs rather than rounding
or wrapping. Use fixed-width 19-digit decimal fields. Range-named objects have no segment ID
or companion empty index file. Timeline objects alone use consecutive
`segments/<segment>` keys. Listing names can build a sorted in-memory range
index without reading payloads. `list_with_offset` supports incremental
listing; finish the enumeration before sorting or advancing a cursor. Do not
assume that it avoids filesystem traversal. Optional `PaginatedListStore`
seeking with `offset` and `max_keys` requires a backend with verified lexical
ordering and offset support; that trait is not a requirement for local stores.

Every PUT is a whole-object create. One writer owns a prefix, so a
create collision is accepted only when the existing bytes equal the intended
object; otherwise fail. Matching bounds alone do not establish identical
groups or payloads, including after an interrupted write. Never rewrite it.
Object attributes such as content type or cache policy are optional hints,
never format metadata.

For `.info`, likewise GET an existing object and require
byte-equivalent contents. A priority or timescale mismatch is a hard enrollment
error, never an idempotent retry.

The store exposes the layout and codec helpers plus put, get, list, and delete
over the underlying `ObjectStore`. There is no `.head`, manifest, `.complete`,
append, or mutable object; listing is the source of truth for bootstrap and
recovery. Do not add presigned-URL handling; credential policy belongs to the
application.

Land the crate in the moq workspace beside `hang`.
