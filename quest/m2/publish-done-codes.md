# [S] PUBLISH_DONE says why a subscription ended

## Goal

A moq-transport PUBLISH_DONE carries GOING_AWAY (0x4), TOO_FAR_BEHIND (0x5), and EXPIRED (0x6) when those are the reason, and a subscriber reads each back as its stream code, in Rust and JS, so the bindings report why instead of an unspecified remote error. A relay that ends a relayed track because its upstream ended (a malformed track it aborted, say) forwards that reason instead of INTERNAL_ERROR.

## Plan

Follow-up of [moq-dev/moq#7396](https://github.com/moq-dev/moq/pull/7396), which did this for MALFORMED_TRACK (0x12). The stream-reset codes already map both ways in `rs/moq-net/src/ietf/error.rs:30-75`; only PUBLISH_DONE doesn't use them.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Reuse #7396's path: the publisher picks the PUBLISH_DONE code from the same error-to-code mapping that resets the track's streams, and the subscriber reads it back as the peer's stream code. Cover drafts 18, 20, and 22, both bindings, and JS, each test failing without its fix. Draft 20 added 2026-11-03 (maintainer, /quest-plan of the follow-ups from [moq-dev/moq#7704](https://github.com/moq-dev/moq/pull/7704)), since draft 20 joined the covered drafts in [interop coverage](/quest/m1/interop-coverage/README.md).

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs): add the relayed case here, from [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135)'s follow-ups. When the relay aborts a malformed upstream track it still ends the downstream subscription with PUBLISH_DONE `INTERNAL_ERROR`, so bindings show `Internal`. Map the upstream end's reason through the same path, after checking whether draft-22's PUBLISH_DONE registry has a code that fits each case. Options shown:

- m0
- m1
- ✅ m2, in this quest

Public API: none. Wire: existing registered codes only.
