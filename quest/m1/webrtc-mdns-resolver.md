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
  the name. A description's lines are already held, bounded by its size.
- Trickled `.local` candidates reach the resolver through a bounded channel,
  so `add_candidate` waits for resolver capacity rather than queueing. Held
  candidates are then bounded by what the peer has sent, and the peer's
  signaling stalls behind its own slow names.
- The cost is one mDNS query in flight per peer, and the work queued behind it
  is bounded by the peer's descriptions and backpressured trickle.

Tests: a description with many `.local` lines sharing one name issues one
query; many distinct names resolve one at a time; sustained trickled distinct
names make `add_candidate` wait instead of queueing; dropping the peer cancels
pending resolution. Use a fake resolver rather than real multicast.

Public API: none. Wire: none.

## Related

- [P2P](/quest/m2/p2p/README.md) - the signaling that exposes this transport to untrusted peers
