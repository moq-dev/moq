# [S] A GOAWAY to a draining fallback session is followed

## Goal

When a WebSocket fallback session drains because a QUIC probe got through,
and its peer then sends a GOAWAY naming another node, the client's next dial
goes to that node, not back to the one that is leaving.

## Plan

Found 2026-11-10 by the reconnect model ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`rs/moq-net/verify/reconnect/RedirectLost.cfg` breaks `RedirectFollowed`.
`probed()` in `rs/moq-net/src/reconnect.rs` drops the fallback session's
GOAWAY watcher along with its `Live` state, so a redirect that arrives while
it drains is lost; if the QUIC handshake then fails, the policy redials the
leaving node until the handover window ends, then reports offline.
`residual_a_redirect_after_a_probe_got_through_is_lost` in
`reconnect/tests.rs` replays it.

Recommended fix: keep the GOAWAY watcher on the predecessor, and while no
replacement serves, act on an accepted redirect the way `end()` does. Flip the
residual test, move `RedirectFollowed` into `Handover.cfg`, delete
`RedirectLost.cfg`, and update the `verify-reconnect` skill.

Also fix the `Backoff` doc in the same file: it promises a GOAWAY back to an
endpoint that already redirected us is never followed at once, but the
`Redirects` cycle check catches a loop only after a lap (A to B, B to C, C to B
is followed once). Reword the doc to what the check does; no code change.

Caused by: [moq-dev/moq#8330](https://github.com/moq-dev/moq/pull/8330), which
added the fallback probe.

Public API: none. Wire: none.
