# [S] moq-native and moq-rtmp log credentials

## Goal

No native log line contains a credential: relay URLs are logged without their
query or userinfo, and an RTMP stream key is never logged at all.

## Plan

`rs/moq-native` logs the full connect URL at info and warn in thirteen places
(`reconnect.rs`, `websocket.rs`, `tcp.rs`, `unix.rs`, and the dial paths of
`quinn.rs`, `quiche.rs`, and `noq.rs`). Only the fingerprint fetch in those
three backends and `Cluster::run_remote_session` strip the query first, each
with its own `set_query(None)` copy. `rs/moq-rtmp/src/dial.rs` logs
`%stream_key` at info when a publish or play is accepted.

- Add one `Display` wrapper for a redacted URL (scheme, host, port, path; no
  query, no userinfo) and use it at every URL log site. Delete the ad-hoc
  `set_query(None)` copies in favour of it.
- Treat the stream key as a secret: log the app and path instead.
- Test: the wrapper's `Display` of a URL carrying `?jwt=` and `user:pass@`
  contains neither.

The browser half of the same finding is
[#2405](/quest/m0/2405-js-net-connect-logs-on-every-connection-at-the-wrong.md).

## Required

- [#2405](/quest/m0/2405-js-net-connect-logs-on-every-connection-at-the-wrong.md) - the issue closes only once both halves are fixed, so the half that closes it goes second

## Closes

- [#2405](https://github.com/moq-dev/moq/issues/2405) - close this issue when the quest finishes
