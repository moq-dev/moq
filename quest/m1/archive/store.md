# [M] Archive store

## Goal

`moq-archive` stores the versioned `(track, segment)` objects of the
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
<prefix>/<encoded-track>/<segment>
```

Implement the percent encoding of track names here; nothing in `rs/hang/src`
does it yet. `.info` is a versioned JSON body with the immutable priority and
timescale. A segment object is the versioned binary envelope: a group/frame
table with timestamps and payload offsets, then the original payload bytes.
Bounds-check every table entry while decoding and refuse an unknown version.

Every PUT is a whole-object create. One writer owns a prefix, so a
deterministic key that already exists means the segment is already persisted;
retries neither compare checksums nor rewrite it. Object attributes such as
content type or cache policy are optional hints, never format metadata.

`.info` is the exception: if it already exists, GET it and require
byte-equivalent contents. A priority or timescale mismatch is a hard enrollment
error, never an idempotent retry.

The store exposes the layout and codec helpers plus put, get, list, and delete
over the underlying `ObjectStore`. There is no `.head`, manifest, `.complete`,
append, or mutable object; listing is the source of truth for bootstrap and
recovery. Do not add presigned-URL handling; credential policy belongs to the
application.

Land the crate in the moq workspace beside `hang`.
