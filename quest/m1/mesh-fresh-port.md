# [S] Mesh dials recover from a stuck flow

## Goal

A relay whose dials to one cluster peer keep failing redials that peer from a
fresh UDP socket and source port, so a network fault that drops a single
5-tuple can't strand the link while other ports would work. A test that drops
one source port's packets shows the link coming back on another.

## Plan

From moq.pro's live fleet on 2026-11-02
([moq-dev/moq.pro#3290](https://github.com/moq-dev/moq.pro/pull/3290)): every
mesh dial from `edge0.bom0` left from one fixed UDP port (51705). Captures on
both ends showed the provider path into Mumbai dropping every reply on the
flow `tyo0:443 -> bom0:51705` for about 27 hours, while fresh source ports to
the same peer completed 8 of 8 QUIC handshakes. The relay never changed port,
so the link stayed down until a restart rebound it.

- Decide between rebinding the shared endpoint after N consecutive failed
  dials to one peer, and a socket per peer. A per-peer socket isolates faults
  but costs a socket and file descriptor per link. Record the choice and why.
- Keep accepted sessions on the existing listener; only outbound dials move.
- Interacts with
  [refusing undialable peers](/quest/m2/relay-undialable-peers.md): a peer
  whose scheme can't be dialed is still refused at startup, not redialed.
