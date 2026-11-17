# [S] An RTSP failure says whether redialing can fix it

## Goal

`moq import rtsp` stops redialing a camera that refused access or offers
nothing publishable, without matching `moq_rtsp::Error` variants. moq-rtsp
reports whether a failure is final, and the CLI's redial loop
(`rs/moq-cli/src/rtsp.rs`) asks it rather than listing variants itself, per
AGENTS.md's "never match on errors". Which failures redial stays as today.

## Plan

Decided 2026-11-16 (maintainer, on
[moq-dev/moq#8939](https://github.com/moq-dev/moq/pull/8939)): the CLI's
`retries` helper, which matches `Unauthorized` and `NoSupportedStream`, goes.

- Add `moq_rtsp::Error::is_final`, shaped like `moq_auth::Error::is_final_grant`
  (from [moq-dev/moq#8933](https://github.com/moq-dev/moq/pull/8933)): the one
  list of failures that asking again cannot fix, kept next to the variants so
  a new one is classified where it is added. It lists `Unauthorized`,
  `NoSupportedStream` and `InvalidUrl`: no redial fixes a URL, though
  `Client::new` already refuses one before the loop, so the loop's behavior
  does not change.
- The CLI redials on `!err.is_final()`, and its `session_failures_retry` test
  moves to moq-rtsp as a test of `is_final`.

Public API: additive on moq-rtsp (`Error::is_final`). Wire: none.
