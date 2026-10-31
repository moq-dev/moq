# [L] Shared group serving and ingest in JS

## Goal

In `js/net`, the moq-lite (`js/net/src/lite/`) and moq-transport
(`js/net/src/ietf/`) sessions share one group serving path and one group
ingest path, mirroring the Rust split from
[Shared group serving and ingest](/quest/m2/shared-serve.md). Each wire plugs
in only its own header and object or frame encoding. Behavior, wire, and
public API are unchanged.

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "cut epoch over
moq-transport"): ✅ Rust first, then JS as its own quest. Follow the seam the
Rust quest settles, so the two languages stay mirrored. Where JS's structure
makes a different seam simpler, take it and record why in the PR.

Tests: the existing `js/net` suites pass unchanged, along with
`just test interop --all`.

Public API: none. Wire: none.

## Required

- [Shared group serving and ingest](/quest/m2/shared-serve.md) - the Rust seam this mirrors
