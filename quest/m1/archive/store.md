# [M] Archive store

## Goal

`moq-archive` stores the same versioned `(track, segment)` objects on memory,
local disk, and S3-compatible storage through a generic
`T: object_store::ObjectStore`.

## Plan

Use `object_store` directly rather than adding a second store trait or separate
backend implementations. Callers that need runtime dispatch can supply
`Arc<dyn ObjectStore>`; the archive API itself remains generic.

The portable layout is:

```text
<prefix>/<encoded-track>/.info
<prefix>/<encoded-track>/<segment>
```

Encode track names with the HANG recording percent encoding. `.info` is a
versioned JSON body containing immutable track priority and timescale. A segment
object is a custom versioned binary envelope containing multiple complete MoQ
groups from one track, a group/frame table with timestamps and payload offsets,
then the original payload bytes. Bounds-check every table entry while decoding.

Every PUT is an atomic whole-object create. Under the recording's unique epoch
and single-writer invariant, an existing deterministic key means the segment is
already persisted; retries do not compare checksums or rewrite it. Object
attributes such as content type or cache policy are optional hints, never format
metadata.

Track metadata is the exception: if `.info` already exists, GET it and require
byte-equivalent contents. A priority or timescale mismatch is a hard enrollment
error, never an idempotent retry.

The store exposes layout/codec helpers plus put, get, list, and delete over the
underlying `ObjectStore`. There is no `.head`, manifest, `.complete`, append,
or mutable object. Listing is the source of truth for bootstrap and recovery.
Do not add presigned-URL handling; credential policy belongs to the application.

Land the crate in the moq workspace beside HANG and the archive draft.
