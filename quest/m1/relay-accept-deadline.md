# [S] A relay's accept deadline outlasts a client's handshake backoff

## Goal

A relay keeps a client on a bad network long enough to finish its QUIC
handshake under the QUIC Interop Runner's 30% loss, so `--listen-timeout`
does not shed the clients moq-quic's own handshake timeout now keeps.

## Plan

Found by [the handshake loss quest](https://github.com/moq-dev/moq/pull/8824),
which raised moq-quic's `handshake_timeout` to 30 s, an inactivity bound.
`moq_net::server::DEFAULT_TIMEOUT` (10 s), which `moq-tokio`'s
`listen.timeout` and so the relay's `--listen-timeout` default to, is a total
deadline from the connection's arrival. It covers the QUIC handshake, the
client's SETUP, and the caller's admission, and for moq-lite 01/02, whose
server SETUP waits on `ok()`, the reply too; every other version sends its
reply from the started session, outside the deadline. Under the QUIC Interop
Runner's 30% loss a client's probe gaps alone reach 13 s, so this deadline
still drops it.

Decided 2026-11-15 (maintainer, /quest-complete): raise the default to 30 s
(rejected: keep 10 s), the same as `moq-tokio`'s connect timeout on the client
side, and update `doc/bin/relay/config.md`, `doc/bin/cli.md`, and
`doc/lib/rs/moq-net.md`.

A total deadline is not an inactivity bound: a client that keeps making
progress but needs more than 30 s overall, such as one configured with a
handshake timeout above 30 s and hitting several long gaps, is still shed.
That is the intended trade: past 30 s the client's own connect timeout gives
up too.

Nothing else bounds a half-open session: `Admission` only counts active
connections for draining and caps none, so this deadline is the whole bound on
how long each one holds its slot, now up to three times longer.

Public API: the default of `moq_net::Server::with_timeout` and `--listen-timeout`,
a behavior change. Wire: none.
