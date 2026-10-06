# [S] Request updates reach their target on drafts 14 to 16

## Goal

On moq-transport drafts 14 and 15, a SUBSCRIBE_UPDATE reaches its
Subscription Request ID. On draft 16, a REQUEST_UPDATE reaches its Existing
Request ID. Today `rs/moq-net/src/ietf/adapter.rs` routes both by the update's
own new Request ID (the first field), so it never reaches its target.

## Plan

Found while working on request caps (#4820); the bug predates it and can be
fixed independently. Route updates by their second field on drafts 14 to
16, and keep the newer drafts' routing as it is. The codec currently stores
that field as `subscription_request_id` on all three affected drafts.
Land a regression test for each affected draft with distinct update and
target IDs, proving the update reaches the named request and failing without
the fix. Rewrite the existing `test_classify_subscribe_update_followup` the
same way rather than keep it beside them: its body carries a single ID, so it
locks in the current routing.

Public API: none. Wire: behaviour within the drafts; no format change.
