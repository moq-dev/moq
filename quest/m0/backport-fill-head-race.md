# [S] Ship the joined-head FETCH race backport

## Goal

The `release` relay no longer races its subscription's joining head with a
cache-miss FETCH, and the fix reaches users in a `moq-net` patch release.
Public API and wire stay unchanged.

## Plan

[moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135) merged on `main`.
The relay fix is hand-ported in [moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680), targeting `release`.

The race reproduced on `release`: both joined-head miss tests sent an extra
FETCH while the subscription's own fill was still outstanding. The older-history
control fetched immediately. The port preserves those cases and covers reader
cancellation and track abort using release's paused-time harness.

Decided 2026-10-30 and reaffirmed 2026-11-01 (maintainer): port only the relay
fix. Release's JS publisher refuses every FETCH, so the old publisher timestamp
bug cannot happen there. The settled main PR also carries only the relay fix.

The port uses release's existing per-group requests and joining `Fill`; it
imports none of main's range-fill or subscription-widening machinery. The
joined group waits for its own head or a settled fill, the cache answers a head
that arrives, and a remaining miss still FETCHes. Older history does not wait.

Local review also exposed a pending miss no longer being polled during the
PUBLISH_DONE grace. The backport adds that poll and a paused-time reproduction.
Main's corresponding grace also omits it. Land that main-side correction before
the backport, following the main-first backport rule.

After the backport lands, use the existing patch-release process. Package
version bumps, tags, and publication are separate from this implementation
attempt. Verify that the published `moq-net` patch contains the backport, then
remove this quest and its links.

## Waiting

- moq-dev/moq: land the PUBLISH_DONE grace correction on main before [moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680) lands; the backport contains the release-side poll and a paused-time reproduction.
- Maintainer: review and merge [moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680) after the release checks pass, then publish the `moq-net` patch through the normal release process; verify its source contains the joined-head wait.
