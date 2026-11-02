# [XS] Identities are percent-encoded in paths

## Goal

Wherever an identity becomes a URL or storage path segment (moq-hls's HLS
URLs and the archive's recording keys, `<path>/@<identity>`), it is
percent-encoded as one segment and decoded on the way back. A stable
identity containing `/`, `?`, `%` or `#` round-trips, and one test per
surface proves it.

## Plan

Planned 2026-10-31 (`/quest-plan`, maintainer), a follow-up reported by the
[Identity and epoch](/quest/m1/api-audit/route/identity.md) implementation.
Minted identities (`~` plus random bits) are URL-safe, so only a stable
identity set by a publisher can break these paths today. Reuse the
percent-encoding the archive already applies to track names (see
[archives](/quest/m1/archive/README.md)) rather than adding a second
scheme. List any other surface that embeds an identity, found with
`git grep`, in the PR.

Public API: none. Wire: none. Existing keys stay readable, since minted
identities encode to themselves.

## Required

- [Identity and epoch](/quest/m1/api-audit/route/identity.md) - the identity these paths embed
