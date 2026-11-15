# [S] A relay's accept deadline outlasts a client's handshake backoff

## Goal

A relay keeps a client on a bad network long enough to finish its QUIC
handshake, as moq-quic's own handshake timeout now does, so `--listen-timeout`
does not shed the clients that timeout serves.

## Plan

Found by [the handshake loss quest](https://github.com/moq-dev/moq/pull/8824),
which raised moq-quic's `handshake_timeout` to 30 s, an inactivity bound.
`moq_net::server::DEFAULT_TIMEOUT` (10 s), which `moq-tokio`'s
`listen.timeout` and so the relay's `--listen-timeout` default to, is a total
deadline from the connection's arrival through the SETUP reply, and it covers
the QUIC handshake too. Under the QUIC Interop Runner's 30% loss a client's
probe gaps alone reach 13 s, so this deadline still drops it.

Recommendation: raise the default to 30 s, the same as `moq-tokio`'s connect
timeout on the client side, and update `doc/bin/relay/config.md`,
`doc/bin/cli.md`, and `doc/lib/rs/moq-net.md`. Each half-open session then
holds its slot up to three times longer, which admission already bounds.

Public API: the default of `moq_net::Server::with_timeout` and `--listen-timeout`,
a behavior change. Wire: none.

## Waiting

- Maintainer: raise the default accept deadline from 10 s to 30 s, or keep 10 s and accept that a relay sheds clients under heavy loss. Changing an admission default trades slowloris exposure for reach, which is the maintainer's policy call. Recommendation: raise it to 30 s.
