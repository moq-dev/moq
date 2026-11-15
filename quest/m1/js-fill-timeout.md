# [XS] JavaScript ignores FILL_TIMEOUT as Rust does

## Goal

A JS publisher accepts any FILL_TIMEOUT a moq-transport subscriber sends, up to
the varint maximum, and ignores it as the Rust publisher does, instead of
refusing the subscription with `malformed(FILL_PARAMETERS)`.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md):
`js/net/src/ietf/filter.ts` and `parameters.ts` decode it with `toNumber`,
which throws above 2^53, while Rust skips it and keeps only whether it was
present (`rs/moq-net/src/ietf/fetch.rs`). Nothing in JS reads `fillTimeout`
since [moq-dev/moq#8445](https://github.com/moq-dev/moq/pull/8445) removed
JS FETCH serving, so keep presence only, delete the numeric field and its
encode path, and flip `js/net/src/integers.test.ts`'s refusal. Also fix
`fetch.rs`'s comment saying the request "is refused", which contradicts
`doc/concept/standard.md` ("accept and ignore").

Public API: none. Wire: none (the JS side stops refusing a valid value).
