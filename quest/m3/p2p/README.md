# P2P

## Goal

Opted-in moq-lite clients serve each other directly, over WebRTC data channels
between any two peers and over iroh between native ones, while the relay stays
the rendezvous and the path of last resort. A client subscribes through the
relay at once, negotiates peers in the background, and a subscription
migrates to a direct peer when that peer is the broadcast's origin (or a
routing node such as `moq-cli --p2p`) and back to the relay when the peer
goes away. A browser never carries another peer's traffic. Nothing waits on ICE and nothing breaks when it
fails: a symmetric NAT, a denied local-network prompt, or a filtered multicast
segment leaves the client exactly where it started.

The application is in charge of policy. It supplies the ICE servers (an empty
list is LAN only: host candidates and nothing else), publishes opaque hints
about itself, decides which roster peers to dial, and sets the roster size
above which new negotiations stop using STUN. P2P is off unless the
application turns it on.

Two endpoint pairs work: browser to browser and browser to native. Native
pairs prefer QUIC: iroh when both advertise an endpoint, a data channel
otherwise.

Non-goals: TURN, since the relay already carries the media as a MoQ route and
relaying ICE traffic would cost the same egress with worse framing; operating
without a relay; RTP media; a channel-per-stream mapping, for the reasons
under "One channel, qmux, ordered".

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer.

### The relay is the rendezvous and the fallback

Every opted-in peer already holds a session to the same relay, so discovery
and signaling ride that session as ordinary moq broadcasts under a reserved
prefix (`.p2p/` by default, configurable). The relay learns nothing new;
trust is its token scope. A peer that may publish
under the prefix is as trusted as any publisher the token admits, so this
line needs no E2EE.

The relay route never goes away. Migration is route ranking in the origin, so
a peer that dies, drains, or was never reachable leaves the relay route as
the best remaining one and the subscription moves back. That is the whole
fallback story; nothing TURN-shaped is built.

A relay that also answers STUN on its QUIC port
([one port](/quest/m2/one-port/README.md)) is the lowest-RTT STUN server a
client can name, but this line takes ICE servers from the application and
works with any.

### Policy is the application's

`Peers` takes the shared origin plus:

- `enabled`: an explicit on/off signal, off by default;
- `prefix`: the reserved prefix, `.p2p/` under the session root by default;
- `iceServers`: the RTCPeerConnection server list; empty means LAN only;
- `max`: the roster size above which a new negotiation is built with an empty
  server list. Host and mDNS candidates are always gathered, so LAN pairing
  never stops; only server-reflexive candidates are withheld. Pairs already
  established are left alone when the roster grows past `max`, which is what
  keeps the boundary from flapping;
- `meta`: an opaque JSON value published in this peer's roster entry;
- `select(peers)`: called on every roster change with each peer's id and
  `meta`, returns the peers to dial. The default dials everyone.

Geo and AS hints are the application's to source (a peer does not know its
own AS); downstream exposes a whoami endpoint for exactly this.

### One channel, qmux, ordered

The transport is qmux over one reliable, ordered data channel, one qmux
record per SCTP message, as the WebSocket binding does. SCTP flow control is
message-based, so `max_record_size` defaults to 16 KiB and never exceeds the
negotiated `maxMessageSize` (256 KiB in Chrome).

The cost is head-of-line blocking: one lost chunk stalls every stream until
SCTP retransmits it. qmux frames already carry stream offsets, so the
follow-up is a qmux transport parameter that permits reordering plus receiver
reassembly, which [qmux on the QUIC core](/quest/m2/quic-qmux.md) provides
for free. Both sides advertise that capability in the roster before the
channel is created so it can run `ordered: false`; a loss then stalls only
the stream it hit. `RTCDataChannel.ordered` cannot change after the
channel exists, so the first qmux record is too late to choose. The
[harness](/quest/m3/p2p/harness.md) supplies the numbers that decide when
that follow-up is worth it.

Channel-per-stream is not planned. moq opens a stream per group, so it churns
against Chrome's 1024-id cap and its close-event id reclaim, needs DCEP per
channel, and has no reset code without a side channel. Unordered qmux gives
the same independence without fighting the browser.

### Routes and migration

Routing is the [cluster routing line](/quest/m1/cluster-routing/README.md)'s:
every session speaks per-node ROUTEs and path-less ANNOUNCEs, so a broadcast's
announce names its origin node, the roster maps that node to a peer, and the
route metric over the node's own link costs picks the direct link or the
relay. The roster id is the node id, held once per origin rather than once
per session.

Decided 2026-10-01: a direct link wins only when the peer is the origin or a
routing node; a watching tab never re-serves what it receives. Offloading a
room of watchers is a customer relay's job (`moq-cli --p2p` is the small
one), which keeps warm and cold costs and cache-aware routing out of the
protocol. [Direct peers win](/quest/m3/p2p/cost-scopes.md) sets the cost
knobs and defaults.

No Rust + WASM in-tab hop: [rs2ts](/quest/m1/rs2ts/remove-wasm.md) removes the
WASM build, so the browser side stays TypeScript.

### Risks

- Chrome 147 prompts for local network access on a dial to a private address
  from a public origin, and the same gate is proposed for WebRTC. A denial
  costs join time, not correctness.
- Firefox bug 1698141: Firefox-initiated LAN P2P to Chrome can fail on mDNS
  candidate parsing.
- Multicast-filtered networks break `.local` resolution; those peers pair
  over server-reflexive candidates or not at all.
- Symmetric NATs on both ends fail ICE without TURN. By design the relay
  keeps serving.

## Required

- [Data channel transport](/quest/m3/p2p/transport.md) - `@moq/p2p` speaks qmux over one ordered RTCDataChannel behind the WebTransport shape `@moq/net` consumes
- [Signaling and policy](/quest/m3/p2p/signal.md) - opted-in peers find each other under the prefix, the application picks who to dial, and the roster-size gate decides whether STUN is used
- [Native data channel transport](/quest/m3/p2p/webrtc.md) - `moq-tokio` holds a moq-net session with a browser over str0m with a full ICE agent
- [moq-cli joins](/quest/m3/p2p/cli.md) - `--p2p` publishes a roster entry with its iroh endpoint, dials iroh between native peers, and serves browsers as a transit hop
- [Direct peers win](/quest/m3/p2p/cost-scopes.md) - a direct link wins only when the peer is the origin or a routing node, by link costs the node sets itself
- [Watch opts in](/quest/m3/p2p/watch.md) - one attribute turns it on in the demo and the watcher migrates to the cheapest route
- [Harness](/quest/m3/p2p/harness.md) - the Playwright harness and the numbers behind every mapping decision
- [Unordered qmux](/quest/m3/p2p/unordered.md) - qmux tolerates reordering so the data channel runs unordered and a loss stalls one stream

## Related

- [Peer grants](/quest/m1/auth/peer-grant.md) - the hop-bound credential a direct session presents; HMAC keys issue none
- [E2EE](/quest/m1/e2ee/README.md) - what a peer would need if the token scope stopped being the trust boundary
