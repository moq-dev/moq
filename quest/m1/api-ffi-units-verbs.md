# [S] moq-ffi durations are microseconds and its verbs match its getters

## Goal

Every bare-integer duration on the moq-ffi records, the C ABI, and the
wrappers is microseconds, and a session's two origins are named the same
way on the getter and the setter. Today `MoqAudioEncoderOutput.frame_duration_us`
sits beside `MoqAudioDecoderOutput.max_age_ms` and `MoqBackoff.{initial_ms,
max_ms, timeout_ms}` while frames carry `timestamp_us`, and
`MoqSession::publisher()` / `consumer()` pair with
`MoqClient::set_publish()` / `set_consume()`.

## Plan

Decided 2026-09-14: microseconds everywhere. `max_age_ms` becomes
`max_age_us` on both decoder outputs, the backoff fields become
`initial_us` / `max_us` / `timeout_us`, and the C ABI struct fields and each
wrapper's documented examples follow (`rs/moq-ffi/src/{audio,video,session}.rs`,
`rs/libmoq/src/api.rs`, `doc/lib/*`). `MoqSession::publisher()` /
`consumer()` become `publish()` / `consume()`. Any moqsink property that
mirrors a renamed field keeps GStreamer's own conventions.

Public API: breaking on moq-ffi, libmoq, and every binding, so on dev.
Wire: none. Run `just test smoke-full` and the obs test.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so the binding records release with one unit
- [Rate estimate names](/quest/m1/api-rate-estimate-names.md) - the other moq-ffi field rename in this cycle
