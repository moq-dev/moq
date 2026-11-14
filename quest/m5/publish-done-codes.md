# [S] PUBLISH_DONE says why a subscription ended

## Goal

A moq-transport PUBLISH_DONE carries GOING_AWAY (0x4), TOO_FAR_BEHIND (0x5), and EXPIRED (0x6) when those are the reason, and a subscriber reads each back as its stream code, in Rust and JS, so the bindings report why instead of an unspecified remote error. A relay that ends a relayed track because its upstream ended with one of those reasons forwards it instead of INTERNAL_ERROR.

## Plan

Follow-up of [moq-dev/moq#7396](https://github.com/moq-dev/moq/pull/7396), which did this for MALFORMED_TRACK (0x12). The stream-reset codes already map both ways in `rs/moq-net/src/ietf/error.rs` (`GOING_AWAY`, `TOO_FAR_BEHIND`, `MALFORMED_TRACK`); PUBLISH_DONE (`Served::status` in `rs/moq-net/src/ietf/publisher.rs`, `PublishDoneStatus` in `ietf/publish.rs`) still sends INTERNAL_ERROR for every error but authorization and MALFORMED_TRACK.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Reuse #7396's path: the publisher picks the PUBLISH_DONE code from the same error-to-code mapping that resets the track's streams, and the subscriber reads it back as the peer's stream code. Cover drafts 18, 20, and 22, both bindings, and JS, each test failing without its fix. Draft 20 added 2026-11-03 (maintainer, /quest-plan of the follow-ups from [moq-dev/moq#7704](https://github.com/moq-dev/moq/pull/7704)), since draft 20 joined the covered drafts in [interop coverage](https://github.com/moq-dev/moq/blob/acef90cf0425d9f0e4e699759cede07adfb2ecfb/quest/m1/interop-coverage/README.md).

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs): add the relayed case here, from [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135)'s follow-ups. A relay already forwards a malformed upstream as MALFORMED_TRACK since #7396; map the other upstream end reasons through the same path, after checking whether draft-22's PUBLISH_DONE registry has a code that fits each case.

Moved to m5 2026-11-12 (maintainer, /quest-audit): moq-transport only, with no named consumer.

Public API: none. Wire: existing registered codes only.
