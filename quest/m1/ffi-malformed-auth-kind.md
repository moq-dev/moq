# [S] Bindings name every session code

## Goal

A session closed with MALFORMED_AUTH_TOKEN (0x16) reports that kind through
moq-ffi's `MoqProtocolKind`, moq-c, and every hand-written wrapper, instead of
`Unknown`. Any other session code moq-net names but the bindings drop (check
0x13 and 0x17) gets the same treatment.

## Plan

- moq-net already has `SessionError::MalformedAuthToken`
  ([moq-dev/moq#6612](https://github.com/moq-dev/moq/pull/6612) follow-up).
- Follow the root Cross-Package Sync table for moq-ffi.
