# P2P

## Goal

Opted-in moq-lite clients serve each other directly, over WebRTC data channels
between any two peers and over iroh between native ones, while the relay stays
the rendezvous and the path of last resort. A client subscribes through the
relay at once, negotiates peers in the background, and each subscription
migrates to a peer when the origin ranks that route cheaper and back to the
relay when the peer goes away. Nothing waits on ICE and nothing breaks when it
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

### The relay is the rendezvous and the fallback

Every opted-in peer already holds a session to the same relay, so discovery
and signaling ride that session as ordinary moq broadcasts under a reserved
prefix (`.p2p/` by default, configurable), the same shape
[carrier voice](/quest/m2/carrier-voice/README.md) uses for call setup. The
relay learns nothing new; trust is its token scope. A peer that may publish
under the prefix is as trusted as any publisher the token admits, so this
line needs no E2EE.

The relay route never goes away. Migration is route ranking in the origin, so
a peer that dies, drains, or was never reachable leaves the relay route as
the best remaining one and the subscription moves back. That is the whole
fallback story; nothing TURN-shaped is built.

A relay that also answers STUN on its QUIC port
([one port](/quest/m1/one-port/README.md)) is the lowest-RTT STUN server a
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
reassembly, which [qmux on the QUIC core](/quest/m1/quic/qmux.md) provides
for free. Both sides advertise that capability in the roster before the
channel is created so it can run `ordered: false`; a loss then stalls only
the stream it hit. `RTCDataChannel.ordered` cannot change after the
channel exists, so the first qmux record is too late to choose. The
[harness](/quest/m1/p2p/harness.md) supplies the numbers that decide when
that follow-up is worth it.

Channel-per-stream is not planned. moq opens a stream per group, so it churns
against Chrome's 1024-id cap and its close-event id reclaim, needs DCEP per
channel, and has no reset code without a side channel. Unordered qmux gives
the same independence without fighting the browser.

### Routes and migration

Rust already does the serving-side work: `best_route` re-runs on every table
change and a live subscription re-splices onto a cheaper route with the same
first hop at a group boundary, while an anonymous chain never wins. The JS
origin re-selects on provider change but ranks newest-first; that is
[route cost in the JS origin](/quest/m1/route-cost.md). The JS handshake
already declares a random hop id, so browser hops are identified; the roster
id is that hop id, held once per origin rather than once per session.

Watcher-to-watcher offload needs a tab to forward what it receives, which the
JS origin does not do today: [transit](/quest/m1/p2p/transit.md).

How a P2P route outranks the relay's is deliberately open. Costs today live
in one scope, a relay mesh pricing its own links; a P2P link is neither free
nor the CDN's egress, and `warm` only accumulates, so a tab forwarding the
relay's route ties the relay and loses on chain length.
[Cost across scopes](/quest/m1/p2p/cost-scopes.md) writes the rule before
the watcher depends on it.

### Fallback: a Rust + WASM in-tab hop

If JS transit or ranking proves hard, the browser side can be moq-net in
WASM: `moq-wasm` already runs it over `web-transport-wasm`. A WASM hop would
hold the relay and peer sessions in Rust, giving one implementation of
signaling, policy, ranking, transit, and migration, and TS apps would connect
to it over an in-memory transport. It costs a web-sys data channel poll
transport, an in-memory bridge into `@moq/net`, and parsing every frame twice
in the tab. Recorded here so that decision is made with numbers, not
re-derived.

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

## Quests

- [Data channel transport](/quest/m1/p2p/transport.md) - `@moq/p2p` speaks qmux over one ordered RTCDataChannel behind the WebTransport shape `@moq/net` consumes
- [Signaling and policy](/quest/m1/p2p/signal.md) - opted-in peers find each other under the prefix, the application picks who to dial, and the roster-size gate decides whether STUN is used
- [Native data channel transport](/quest/m1/p2p/webrtc.md) - `moq-tokio` holds a moq-net session with a browser over str0m with a full ICE agent
- [moq-cli joins](/quest/m1/p2p/cli.md) - `--p2p` publishes a roster entry with its iroh endpoint, dials iroh between native peers, and serves browsers as a transit hop
- [Transit in the JS origin](/quest/m1/p2p/transit.md) - a tab forwards the routes it receives to its peers with split horizon and its hop id appended
- [Cost across scopes](/quest/m1/p2p/cost-scopes.md) - the written rule for how relay egress, mesh links, and P2P links compare, so a peer that already carries a broadcast wins
- [Watch opts in](/quest/m1/p2p/watch.md) - one attribute turns it on in the demo and the watcher migrates to the cheapest route
- [Harness](/quest/m1/p2p/harness.md) - the Playwright harness and the numbers behind every mapping decision
- [Unordered qmux](/quest/m1/p2p/unordered.md) - qmux tolerates reordering so the data channel runs unordered and a loss stalls one stream

## Related

- [Peer grants](/quest/m1/auth/peer-grant.md) - the hop-bound credential a direct session presents; HMAC keys issue none
- [Route cost in the JS origin](/quest/m1/route-cost.md) - the watcher-side route pick this line needs
- [One port](/quest/m1/one-port/README.md) - the relay answers STUN on its QUIC port
- [E2EE](/quest/m1/e2ee/README.md) - what a peer would need if the token scope stopped being the trust boundary
- [qmux on the QUIC core](/quest/m1/quic/qmux.md) - the stream core the unordered follow-up rides
- [Carrier voice](/quest/m2/carrier-voice/README.md) - signaling as an application protocol over moq, the pattern reused here
