# [S] /fetch answers from the announce sync instead of waiting

## Goal

The relay's `/fetch` route answers 404 as soon as its origin's announcements for the broadcast are synced and the broadcast isn't announced, and fetches at once when it is, instead of waiting up to 30 s for a broadcast that may never appear.

## Plan

Decided 2026-10-30 (maintainer, follow-up of [moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366)): `/fetch` should not wait for announcements when the Sync code can tell that no more are coming.

- Today `serve_fetch` (`rs/moq-relay/src/web.rs:1096-1105`) waits on `following.available()` within a 30 s deadline, so a missing broadcast hangs the request for 30 s. Race it against `AnnounceConsumer::poll_synced` (`rs/moq-net/src/model/origin.rs:7142`) for the token's broadcast scope: synced with no route is a 404.
- Sync is relative to each session's peer: a relay answers with what it holds without waiting on its own upstreams, so a broadcast known only further upstream returns 404 rather than waiting. That's accepted. Versions that can't mark the boundary (moq-lite-03/04, moq-transport without MoQ Active Count) count as synced once the peer accepts the request, as `poll_synced` documents.
- The 30 s deadline stays fixed and keeps bounding the group fetch and the response body's frame reads. Making it configurable was not wanted (2026-10-30).
- Tests: a fetch for an unannounced broadcast on a synced origin returns 404 without advancing the clock to the deadline. A fetch whose broadcast is announced after the request but before sync still serves the group. A fetch while an upstream session hasn't synced yet waits for that sync.

Public API: none. Wire: none.
