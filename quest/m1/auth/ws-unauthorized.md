# [M] The relay serves WebSocket sessions through moq-tokio

## Goal

A relay refuses a WebSocket client the same way it refuses a QUIC one: as a
session-level close with the same code, which the browser can read. A bad
token over WebSocket ends in `Unauthorized`, the JS client stops
reconnecting, and `connection.error` is set.

## Plan

`rs/moq-relay/src/websocket.rs` admits before the upgrade and answers a
refusal with HTTP 401, 403, or 502, which browsers hide, so the client sees a
generic failure and retries. moq-tokio's own WebSocket listener already
upgrades, reads SETUP, and then admits; the relay is the outlier.

Decided (2026-10-04): the relay hands the upgraded socket to moq-tokio's
`Request`, so `Connection::run` serves WebSocket as it serves QUIC, and the
relay's duplicate admit and supervise loop is deleted. Every refusal becomes
an in-band close, mapped as the io_uring path does: 401 and 403 to
`Unauthorized`, 502 to `App(502)`. An expired token keeps its own
`Error::Expired` path ([expired error](/quest/m1/auth/expired-error.md)). It admits after SETUP, which
[token in band](/quest/m1/auth/token-in-band.md) needs anyway. It needs a new
public moq-tokio constructor for an already-upgraded socket; keep it minimal.

Check that Rust clients, which treat HTTP 401 as terminal today, treat the
session close the same way. The 401 in `doc/bin/relay/auth.md` describes the
auth webhook, not the relay's answer; leave it.

Admitting after SETUP also closes two gaps the 2026-10-07 audit found in the
current pre-upgrade admit, which has no role and no SETUP token: a session
whose role its grant cannot serve (a publisher holding a subscribe-only
grant) is upgraded and its broadcasts silently go nowhere, where QUIC refuses
it in `Cluster::scope`; and a moqt-over-WebSocket client that authenticates
only with the SETUP `AUTHORIZATION TOKEN` is treated as anonymous. Neither
grants extra access, so this stays in m1 (decided 2026-10-07). Serving
through `Connection::run` already applies `request_for` and `Cluster::scope`,
so both close with no relay-side admit; the tests below pin them.

Tests: a WebSocket connect with a bad token ends in a JS `SessionError` with
the unauthorized code and the reload loop stops; a Rust client stops too. A
publish-role WebSocket session with a subscribe-only grant is refused, and a
SETUP-token-only client is admitted with that token's grant.

## Closes

- [#4786](https://github.com/moq-dev/moq/issues/4786) - close this issue when the quest finishes

## Related

- [Expired token error](/quest/m1/auth/expired-error.md) - builds on a session-level refusal on both transports
