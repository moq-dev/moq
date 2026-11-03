# [S] Settle native decoder setup refusal policy

## Goal

Settle which native decoder-open failures are an unsupported media configuration,
and whether that setup must precede resolving a rendition's foreign broadcast,
then finish [moq-dev/moq#7730](https://github.com/moq-dev/moq/pull/7730).
The typed binding error, wrapper support, and OBS ranked fallback are implemented;
later async peer or decode failures remain outside this quest.

## Plan

The user chose a distinct `MoqError` variant returned by audio/video subscribe,
mirrored in moq-c and the wrappers, with OBS falling through on that error and
removing its duplicated audio codec gate (2026-10-31).

The current implementation tags decoder/output setup at the Rust consumer boundary.
Peer retention refusals and worker startup/loss retain separate errors. A malformed
AAC AudioSpecificConfig is a setup refusal regardless of how many decoder backends
are available. OBS owns one ranked catalog snapshot per selection and tries each
candidate once; it does not permanently cache refusals across catalog updates.

Local review exposed two choices the existing error/API shapes cannot distinguish.
`moq-video`'s backend selector aggregates unavailable capabilities and device/resource
failures into `NoDecoder`; classifying only static unsupported configurations requires
preserving that distinction before aggregation. `MoqAudioConsumer::subscribe` resolves
before opening its decoder, so an unsupported configuration referencing an unannounced
foreign broadcast can wait before the decoder gets to refuse it. The current broadcast
is already pinned, and rejected local configurations never subscribe to a media track.
A disposable synchronous probe would open a decoder twice; a prepared decoder handle
could establish support once and be reused after resolution, but adds public API.

Public API: adds `MoqError::UnsupportedConfig`, matching Rust consumer errors and
binding variants, plus moq-c's typed status. Wire: none.

## Waiting

- Maintainer: Choose the decoder-open failure boundary. (1) Keep the attempt-scoped `UnsupportedConfig` for local codec/backend-open refusals, preserving peer and worker failures (recommended, the current implementation). (2) Reserve it for known unsupported codec/layout/description and keep device/resource failures distinct; this needs a typed backend error taxonomy before `NoDecoder` aggregation. Record the choice on [moq-dev/moq#7730](https://github.com/moq-dev/moq/pull/7730), then adjust code and review against it.
- Maintainer: Choose whether local setup must precede foreign broadcast resolution. (1) Keep subscribe's existing resolve-then-open shape with one authoritative decoder open (recommended for this quest's current scope). (2) Split preparation from subscription using a reusable decoder handle, so unsupported foreign renditions fail before any origin request. Settle that public shape before implementing it; avoid a separate check that opens a throwaway decoder and then opens it again.

## Related

- [moq-dev/moq#7331](https://github.com/moq-dev/moq/pull/7331) introduced OBS's duplicated audio codec gate.
