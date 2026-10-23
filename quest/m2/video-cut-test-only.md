# [S] Drop moq-video's test-only cut requests

## Goal

moq-video's encode producer carries no code only its tests use: the keyframe
`Requests` and cut counting behind the private capture `Control` are gone.

## Plan

After [Media surface trim](/quest/m1/api-audit/media-trim.md) makes capture
`Control` private, `Requests` and the cut counting in
`rs/moq-video/src/encode/producer.rs` serve only tests. Delete them, fold the
driver into `publish_capture`, and rewrite the tests against the public
path.

Public API: none. Wire: none.

## Required

- [Media surface trim](/quest/m1/api-audit/media-trim.md) - makes `Control` private
