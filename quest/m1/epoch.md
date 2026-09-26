# [M] Broadcast epoch primitive

## Goal

`moq-net` and `@moq/net` own one `Epoch` type: a publisher instance's
identity, carried as the last path segment `@<uuidv7>` (for example
`demo/BBB.hang/@0192a3f0-...`). It mints, parses, orders (newest greatest),
and reports its wall-clock time. A path splits into its epoch-free name and
an optional epoch. The epoch rides in the path, so it survives any
moq-transport relay with no wire change.

This is the shared primitive the [broadcast epochs](/quest/m1/broadcast-epoch/README.md)
line builds on, and it replaces the e2ee-local `moq_e2ee::Epoch`.

## Plan

- Move `rs/moq-e2ee/src/epoch.rs` into `moq-net` and add the JS equivalent.
  Parsing is strict: `@` followed by a lowercase hyphenated UUIDv7. Nothing else
  counts as an epoch, so an app's own UUID segments never parse as one.
- Path helpers split a path into name and epoch and join them back. A segment
  like `@alice` is valid today and stays valid: strict parsing already keeps it
  from reading as an epoch. Rejecting it instead would break the path contract
  and land on `dev`. Check how the split interacts with
  [path patterns](/quest/m1/auth/patterns.md) and
  hidden broadcasts (a leading `.`, see `doc/concept/moq-lite.md`).
- `moq-e2ee` uses the shared type. Update
  [draft-lcurley-moq-e2ee](/drafts/draft-lcurley-moq-e2ee.md) so the path is
  `<opaque>/@<epoch>`. Keep the HKDF input as the UUID text, so vectors change
  only if they encode the path. Drop "deliberately e2ee-only" from the e2ee
  README.
- Golden cross-language vectors for parse, reject, order, and time, beside
  the existing path tests.
- Nothing mints by default here. Publish and consume behavior is
  [origin](/quest/m1/broadcast-epoch/origin.md).

Public API: additive `Epoch` and path helpers in both languages. The
`moq_e2ee::Epoch` move is a break on an unpublished crate. Wire: none.

## Related

- [E2EE](/quest/m1/e2ee/README.md) - the first consumer, whose TypeScript core requires this
