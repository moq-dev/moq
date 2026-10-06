# [S] The relay configures per-session request limits

## Goal

`moq-relay` sets moq-net's `session::Limits` from its config, with a tighter
default for client sessions than for cluster peers, and documents it in
`doc/bin/relay/`. The bindings report a refused request as its own error
kind instead of mapping `TooManyRequests` to Unknown.

## Plan

Request caps (#4820) put generous defaults in moq-net (100,000 announces and
10,000 subscriptions per session), sized for relay meshes, so viewer
sessions get loose limits. The relay knows which sessions are peers, so it
picks the limit per session. Propose the flag and TOML names in the PR.

Add the error kind to `rs/moq-ffi` and every wrapper per the Cross-Package
Sync table.

Public API: a relay config field and flag, and a new error kind in moq-ffi
and the bindings. Wire: none.
