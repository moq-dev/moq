# [S] moq-rtsp stall tests on a mocked clock

## Goal

`a_silent_session_stalls` and `rtcp_alone_stalls`
(`rs/moq-rtsp/src/client.rs:938`, `:954`) and the two `keyframe_timeout`
tests (`:979`, `:1096`) never fail under load.

## Plan

They set 200 ms timeouts against a real-socket fake server on wall time.
`client.timeout` also bounds every RTSP request step (`within()` at `:266`),
so a loaded runner can stall during setup instead of at the point under
test. Drive the timeouts with a `moq_clock::Manual` clock; moving the fake
server off real sockets alone would leave them on wall time.

Public API: none. Wire: none.
