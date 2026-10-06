# [S] SUBSCRIBE_UPDATE reaches its subscription on drafts 14 to 16

## Goal

On moq-transport drafts 14 to 16, a SUBSCRIBE_UPDATE changes the
subscription it names. Today `rs/moq-net/src/ietf/adapter.rs` routes it by
its own new request ID (its first field), so the update never reaches its
subscription.

## Plan

Found while landing request caps (#4820); the bug predates it. Route a
drafts 14 to 16 SUBSCRIBE_UPDATE by the subscription request ID it carries,
and keep the newer drafts' routing as it is. Land a regression test that
sends an update on each affected draft and fails without the fix.

Public API: none. Wire: behaviour within the drafts; no format change.
