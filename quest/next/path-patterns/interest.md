# [M] Carry pattern grants in moq-lite-06 AUTH

## Goal

AUTH grants on moq-lite-06 carry the shared pattern semantics, without
changing older protocol versions. Interest stays a prefix: [#3770](https://github.com/moq-dev/moq/pull/3770)
keeps patterns off the announce wire, so ANNOUNCE_REQUEST and
SUBSCRIBE_NAMESPACE carry the prefix the caller asked for and a wildcard is
an optional filter on the consume side.

## Plan

Replace lite-06 AUTH grant prefixes with patterns in Rust and JavaScript in
the same change. Update the lite draft and version-gated fixtures together.
Authorize by exact containment in the subscriber's v1 grant. Older moq-lite
versions keep their existing prefix wire and behavior; a grant they cannot
represent is refused, not narrowed. Cluster peers adopt nothing as a side
effect of this wire work.

Test Rust and JavaScript interop, leading wildcards, `**` zero-segment
matches, containment refusal, and old-version behavior.

## Required

- [Lite auth](/quest/next/auth/lite.md) - establish the AUTH exchange before upgrading its grants to patterns
