# [S] Relay exposes a verified peer address

## Goal

An embedder of moq-relay's routes, including WebRTC (WHIP/WHEP), can read the
verified socket address of the client behind each session, never a forwarded
header. Today `listener::Peer` is crate-private and moq-rtc exposes no remote
address, while native QUIC already offers `remote_addr()`.

## Plan

Open question for the maintainer: the API shape (a request extension, a
field on the session, or a getter beside `remote_addr()`). Recommendation: a
public request extension, mirroring `MtlsPeer`, which embedded routes already
extract for the client certificate; it needs no change to the session types.
Keep it to one accessor that every transport fills.
