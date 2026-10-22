# [S] One mDNS resolver per WebRTC peer

## Goal

An untrusted offer or answer cannot make the native WebRTC transport spawn
unbounded work. Each peer resolves its `.local` candidates in one task it
owns, one distinct name at a time, and that task ends with the peer.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer.
`split_mdns` (`rs/moq-tokio/src/webrtc.rs`) collects every `.local` candidate
line, and `Peer::resolve` spawns one detached task per line. Each task waits up
to `MDNS_TIMEOUT` (5 s) and multicasts a query. One description can therefore
create any number of tasks and queries, and they outlive the peer.

- ✅ Bound the cost by design, not with a candidate cap: the `Peer` owns one
  resolver task, aborted on drop. That task dedupes lines by host name, resolves
  each distinct name once, sequentially, and sends every line that shares
  the name. Trickled `.local` candidates join the same queue rather than
  resolving inline.
- The cost is one mDNS query in flight per peer, so total work scales with
  the peers signaling already admits.

Tests: a description with many `.local` lines sharing one name issues one
query; many distinct names resolve one at a time; dropping the peer cancels
pending resolution. Use a fake resolver rather than real multicast.

Public API: none. Wire: none.

## Related

- [P2P](/quest/m2/p2p/README.md) - the signaling that exposes this transport to untrusted peers
