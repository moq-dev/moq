# [S] Audit retention defaults and propose a finite max-age

## Goal

Every max-age and cache-retention default in Rust, JS, and FFI is listed with where it applies (producers, relay cache, the IETF absent-MAX_AGE meaning), and the maintainer has a concrete proposal for a finite default to decide on, so no retention is unlimited by accident.

## Plan

Decided 2026-10-30 (maintainer): asked whether max-age should default to 0 instead of infinite, like max_delay, which [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403) settled (JS and Rust default 0; unlimited is explicit `None`/`null`). For max-age, audit first and propose.

- Inventory each default with path:line: `track::Producer`/catalog/media producers, the relay cache's eviction and pool, JS `maxAge` (omitted means no publisher limit in `@moq/net`'s `Info`, while `@moq/hang`'s `trackInfo` substitutes 30 s, matching `hang::container::MAX_AGE`), FFI, and the IETF publisher treating an absent MAX_AGE as unlimited (wire semantics; likely kept).
- Say what 0 would break: late joiners, fetch of a non-latest group, DVR/archive.
- Propose a default with its reason and the migration; the maintainer decides, then a follow-up implements it.

Public API: none in this quest. Wire: none.
