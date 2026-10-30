# [M] Drop iroh

## Goal

moq-tokio, moq-relay, and moq-cli have no `iroh` feature, the workspace has no
web-transport-iroh or upstream noq-proto dependency, and the docs no longer
describe an iroh transport.

## Plan

Decided by the maintainer on 2026-10-26: iroh and noq hard-wire aws-lc-rs or
ring (noq calls `aws_lc_rs::hmac` directly), so the opt-in feature would keep C
in the graph after the rind migration. It is deleted, with rind, in m2, along with
moq-tokio's `iroh::session` impl of moq-net's traits.

Remove the backend in moq-tokio, the `iroh` features in moq-relay and moq-cli,
the workspace entries for web-transport-iroh and noq-proto, and the MSRV floor
iroh set. Update the docs that describe it (`doc/concept/transport.md`,
`doc/bin/relay/`, `doc/lib/rs/index.md`, and the others `rg -il iroh doc`
finds) and fix quests that mention the iroh backend.

Public API: moq-tokio's, moq-relay's, and moq-cli's `iroh` feature and the
iroh config and URL handling are removed (breaking); migrate by connecting over
QUIC or WebTransport. Wire: none.
