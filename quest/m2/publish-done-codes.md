# [S] PUBLISH_DONE says why a subscription ended

## Goal

A moq-transport PUBLISH_DONE carries GOING_AWAY (0x4), TOO_FAR_BEHIND (0x5), and EXPIRED (0x6) when those are the reason, and a subscriber reads each back as its stream code, in Rust and JS, so the bindings report why instead of an unspecified remote error.

## Plan

Follow-up of [moq-dev/moq#7396](https://github.com/moq-dev/moq/pull/7396), which did this for MALFORMED_TRACK (0x12). The stream-reset codes already map both ways in `rs/moq-net/src/ietf/error.rs:30-75`; only PUBLISH_DONE doesn't use them.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Reuse #7396's path: the publisher picks the PUBLISH_DONE code from the same error-to-code mapping that resets the track's streams, and the subscriber reads it back as the peer's stream code. Cover drafts 18 and 22, both bindings, and JS, each test failing without its fix.

Public API: none. Wire: existing registered codes only.

## Required

- [A malformed track says so downstream](/quest/m2/malformed-track-status.md) - its PR, #7396, adds the PUBLISH_DONE code path this extends
