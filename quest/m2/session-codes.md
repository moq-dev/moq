# [M] Session close codes name only moq-lite's registry, and errors classify themselves

## Goal

`moq_net::SessionError` has a variant only for a code in moq-lite's registry.
The seven codes only moq-transport defines decode to
`SessionError::Unknown(code)`, which already keeps the number, and are gone
from moq-ffi's `MoqProtocolKind`, moq-c's `MOQ_PROTOCOL_KIND_*`, and JS's
session codes. `moq_net::Error` has a public method that answers what a
caller does next (retry on the same source, find another route, or stop), so
no crate matches error variants to decide, and moq-gst uses it.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ the
seven moq-transport-only codes decode to `Unknown(code)` and a classification
method becomes public (rejected: also folding the flat `Error` variants into
the two registries now; keeping the variants). AGENTS.md says never to add
API for functionality only moq-transport has, and never to match on errors;
today the first forces the second.

Facts found by the audit, checked at `80ca9787e3`:

- The seven are `InvalidRequestId`, `KeyValueFormatting`, `InvalidPath`,
  `AuthTokenCacheOverflow`, `UnknownAuthTokenAlias`, `InvalidAuthority`, and
  `TooManyRequestUpdates` (`rs/moq-net/src/error.rs:90,39,94,57,69,98,74`).
  None is in the lite-07 registry in `drafts/draft-lcurley-moq-lite.md`,
  which removed KEY_VALUE_FORMATTING_ERROR in lite-07. Six were added in the
  window.
- Each is mirrored into moq-ffi (`rs/moq-ffi/src/error.rs`) and moq-c
  (`rs/moq-c/src/error.rs`), 7 lines each, and JS lists them in
  `js/net/src/error.ts:43-63`.
- The only classification, `Error::retryable`, `unroutable`, and `transient`
  (`rs/moq-net/src/error.rs:609-620`), is `pub(crate)`. moq-gst rebuilt it by
  matching variants, two of the seven among them
  (`rs/moq-gst/src/source/imp.rs:1034-1049`, `source_lost`).
- `Error` says "unauthorized" three ways (`Error::Unauthorized`,
  `Session(SessionError::Unauthorized)`, `Stream(StreamError::Unauthorized)`),
  and its doc defers folding them to "a later decision"
  (`rs/moq-net/src/error.rs:342`). That fold stays deferred.

For the PR:

- The moq-transport layer keeps a private table to send these codes; only
  decoding and the public enum change. A code received from a peer is
  logged with its number.
- Design the public method from its callers, not from the private three:
  moq-gst asks "did the source go away, or did it refuse me", and resume
  asks "same source or another route". Recommended: one method returning a
  small `#[non_exhaustive]` enum (for example `Retry::{Same, Reroute, Never}`)
  rather than three booleans, mirrored by JS's `routeFailed`
  (`js/net/src/resume.ts`). Survey every match on a `moq_net::Error` variant
  outside moq-net first (about 50 lines by grep) and move each caller the
  method answers; list the ones it cannot answer in the PR.
- moq-gst's `source_lost` and `catalog_lost` become calls to it.
- Mirror the removal in moq-ffi, moq-c, each wrapper's error type, and JS.

Tests: each of the seven code numbers received on a moq-transport session
closes it with `Unknown(code)` and the same classification as before; moq-gst
holds its pads on a lost source and fails on a refusal, as today.

Public API: breaking in Rust (`SessionError` variants), moq-ffi
(`MoqProtocolKind`), moq-c (`moq.h` constants), the wrappers, and JS; adds
the classification method. Wire: none.
