# [L] Carry pattern interest in moq-lite-06

## Goal

An ANNOUNCE_REQUEST on moq-lite-06 carries the full shared pattern instead of a
prefix, without changing older protocol versions.

## Plan

Replace the lite-06 request prefix with a pattern in Rust and JavaScript.
Authorize it by exact containment in the subscriber's v1 grant, derive its
literal head for traversal, and filter announcements with the shared matcher.
Preserve exact set-valued rebasing through rooted views.

Older moq-lite versions keep their existing prefix wire and behavior. For IETF
MoQ interop, request the longest literal head expressible by that protocol and
filter the received announcements locally. An empty literal head requests the
root.

This quest supplies a filter primitive only. Cluster peers adopt scoped
interest only if relay-memory measurements justify it; they do not change as a
side effect of this wire work.

Test Rust and JavaScript interop, leading wildcards, `**` zero-segment matches,
root matches after rebasing, containment refusal, old-version behavior, and the
IETF over-request plus local-filter fallback.

## Required

- [Matcher](/quest/m2/path-patterns/matcher.md)
- [Origin scopes](/quest/m2/path-patterns/origin.md)
