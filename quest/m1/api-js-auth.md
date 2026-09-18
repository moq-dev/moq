# [S] @moq/auth reads like moq-auth

## Goal

`@moq/auth` (renamed from `@moq/token`, so this is the one release that
reshapes it for free) exposes `Key` and `KeySet` namespaces with the verbs
`moq_auth::Key` and `KeySet` have, instead of the `load`/`loadSet`/
`loadPublic`/`sign`/`signWith`/`verifyWith` free-function family.

## Plan

`Key.{parse, public, sign, verify, generate}` and
`KeySet.{parse, public, find, sign, verify}` over the existing zod types,
matching `Key::{from_str, to_public, sign, verify, generate}` and
`KeySet::{from_str, to_public_set, find_key, sign, verify}`; the `With`
suffix is the pattern the naming rules ban. The CLI keeps its verbs and
takes the Rust flag spellings. moq.pro's API mints through `jose` today, so
its churn is nil.

Public API: breaking on @moq/auth, so on dev. Wire: none.

## Related

- [Auth contract](/quest/m1/auth-contract.md) - the Rust side of the same surface
