# [M] noq releases an endpoint's socket on close

## Goal

A noq endpoint can close its UDP socket, and report when it has, without
waiting for every connection handle to drop. moq-tokio then deletes the
closable socket wrapper that #4087 added to make `Listener::close` release the
port.

## Plan

In moq-dev/noq, add an endpoint operation that closes the endpoint, waits
until each connection has sent its close, and then releases the socket. Later
sends are dropped and receives end. Test it there. Cut a noq release, bump the
pin, and replace moq-tokio's wrapper with the upstream call. The Go
`TestReconnectAcrossRelayRestart` and moq-tokio's
`close_releases_quic_socket` stay green.

## Related

- [Listener close](https://github.com/moq-dev/moq/pull/4087) - the local wrapper this replaces
