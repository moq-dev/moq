# [XS] Instances are percent-encoded in paths

## Goal

Wherever an instance becomes a URL or storage path segment (moq-hls's HLS
URLs and the archive's recording keys, `<path>/@<instance>`), it is
percent-encoded as one segment and decoded on the way back. A stable
instance containing `/`, `?`, `%` or `#` round-trips, and one test per
surface proves it.

## Plan

Planned 2026-10-31 (`/quest-plan`, maintainer), a follow-up reported by the
[Instance and epoch](https://github.com/moq-dev/moq/pull/7489) implementation.
Minted instances (`~` plus random bits) are URL-safe, so only a stable
instance set by a publisher can break these paths today. Reuse the
percent-encoding the archive already applies to track names (see
[archives](/quest/m1/archive/README.md)) rather than adding a second
scheme. List any other surface that embeds an instance, found with
`git grep`, in the PR.

Public API: none. Wire: none. Existing keys stay readable, since minted
instances encode to themselves.
