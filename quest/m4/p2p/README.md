# P2P

## Goal

Opted-in moq-lite clients serve each other directly, over WebRTC data channels
when a browser is involved and over QUIC on an ICE path between native ones,
while the relay stays
the rendezvous and an available fallback route. A client subscribes through the
relay at once, negotiates peers in the background, and a subscription
migrates when an application-selected direct connection wins the shared
priority/cost ordering, and returns to the best eligible route when it goes
away. A peer serves its own publications or is an authorized native routing
node such as `moq-cli --p2p`. A browser never carries another peer's traffic. Nothing waits on ICE and nothing breaks when it
fails: a symmetric NAT, a denied local-network prompt, or a filtered multicast
segment leaves the client exactly where it started.

The application is in charge of policy. It supplies the ICE servers (an empty
list disables ICE servers: only host and mDNS candidates, which pair on a
LAN when the network allows it, and can still pair across the Internet
when a host address, often IPv6, is publicly reachable), publishes opaque hints
about itself, decides which roster peers to dial, and may pass a peer an empty
`iceServers` list, for example past a roster size it picks. P2P is off unless the
application turns it on.

Three endpoint pairs work: browser to browser and browser to native over a
data channel, and native to native over QUIC on the ICE-nominated path.
Decided 2026-10-13: no iroh. It brings discovery and relaying of its own
beside the relay; every pair uses the same STUN and ICE, with the relay as
rendezvous and fallback.

Non-goals: TURN, since the relay already carries the media as a MoQ route and
relaying ICE traffic would cost the same egress with worse framing; operating
without a relay; RTP media; a channel-per-stream mapping, for the reasons
under "One channel, qmux, ordered".

## Plan

Deferred to m2 by the maintainer on 2026-10-15, with signaling, transports,
policy, and validation together. CDN-connected clients using selected peer
paths remain the consumer. Reuse the shared routing contracts; no second
routing implementation. Any incompatible wire additions after lite-07
finalizes use a later wip/version.

This epic owns practical P2P setup guidance extending the routing concept:
keep the CDN session while selecting peers, explain no browser transit, and
show application-selected ICE servers including an opt-in relay STUN endpoint
(`stun:relay.example.com:3478`). Link `/bin/relay/config#stun` and the
JS/native P2P reference. Do not imply that direct always beats CDN.

### The relay is the rendezvous and the fallback

Every opted-in peer already holds a session to the same relay, so discovery
and signaling ride that session as ordinary moq broadcasts under a reserved
prefix (`.p2p/` by default, configurable). The relay learns nothing new;
trust is its token scope. A peer that may publish
under the prefix is as trusted as any publisher the token admits, so this
line needs no E2EE.

The CDN session and discovery remain connected. Release unused CDN media
demand after a same-epoch handoff, except bounded overlap while validating a
replacement. If a peer dies, drains, or was never reachable, the shared
origin selects the best remaining eligible route. This is ordinary transport
fallback; nothing TURN-shaped is built. Detecting a delivery stall on a
still-open route is out of scope. Epochless source changes restart instead of resume.

The relay is the nearest STUN server a client can name, but this line takes
ICE servers from the application and works with any. Decided 2026-10-08: the
public STUN responder moved out of the [one-port](/quest/m3/one-port/README.md)
UDP demux into this line, since P2P is its only consumer. Decided 2026-10-13:
[Relay STUN](/doc/bin/relay/config.md#stun) runs on its own configured port,
off by default, and used only when the application lists it.

### Policy is the application's

`Peers` takes the shared origin, plus the knobs below (decided 2026-11-07,
maintainer, /quest-plan from the API sweep of open PRs and remaining quests:
no `max` roster gate, and the on/off `enabled` is the element's attribute,
not a `Peers` knob):

- `prefix`: the reserved prefix, `.p2p/` under the session root by default;
- `iceServers`: the default RTCPeerConnection server list, which each
  selected or admitted peer may override with its own; empty disables ICE
  servers (no STUN or TURN), which is not a LAN-only guarantee.
  Host and mDNS candidates are gathered where the browser's privacy policy
  allows, so LAN pairing stays possible where local-network access and mDNS
  work, with the relay as fallback otherwise,
  and changing a peer's list never closes an established pair;
- `meta`: an opaque JSON value published in this peer's roster entry;
- application-owned peer selection: discovery exposes candidates and only
  explicitly selected peers are dialed; incoming connections need application
  admission too. No default dial-all or automatic
  demand-selected dialing. Prefer an explicit set/handle over a policy callback;
  settle the concrete public shape with its consumer before implementation.
- application-owned per-connection priority, whether CDN or P2P, followed by
  configured additive cost. Discovery and transport do not decide route rank.

Geo and AS hints are the application's to source (a peer does not know its
own AS); downstream exposes a whoami endpoint for exactly this.

### One channel, qmux, ordered

The transport is qmux over one reliable, ordered data channel, one qmux
record per SCTP message, as the WebSocket binding does. SCTP flow control is
message-based, so `max_record_size` defaults to 16 KiB and never exceeds the
negotiated `maxMessageSize` (256 KiB in Chrome).

The cost is head-of-line blocking: one lost chunk stalls every stream until
SCTP retransmits it. Decided 2026-10-08: the channel is always ordered, and
unordered qmux was dropped. Receiver reassembly would exist only in the Rust
qmux, so it could cover native pairs alone, and a native pair runs QUIC,
whose streams are independent. The
[harness](/quest/m4/p2p/harness.md) reports the stall so the cost stays
visible.

Channel-per-stream is not planned. moq opens a stream per group, so it churns
against Chrome's 1024-id cap and its close-event id reclaim, needs DCEP per
channel, and has no reset code without a side channel.

### Routes and migration

Routing is the [cluster routing line](/quest/m3/cluster-routing/README.md)'s:
every session speaks per-node ROUTEs and path-less ANNOUNCEs, and the
application priority followed by total cost picks the direct link or relay.

A routing node id is declared by its origin and unverified (decided
2026-10-26, maintainer, dropping receiver-stamped per-session identities).
Switching between a CDN and a direct path resumes only across a matching
explicit epoch.

Decided 2026-11-02 (maintainer, /quest-unblock): P2P trusts its peers. Joining the prefix takes the
relay's token, so a peer is as trusted as any publisher that token admits,
and a direct session serves without a peer grant or key proof. Peer grants and
P2P identity planning are dropped until a real use case asks for proof. Each
peer's roster `<id>` is its public key, like an iroh endpoint id; nothing proves
it yet. Which roster peer serves an announce stays the application's explicit
selection. If identity or signing is wanted later, moq-lite-07's route
identity becomes a public key, independent of P2P.

Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing): a browser
that forwards for its peers on a local network is planned separately, in m5
behind the Rust-to-TypeScript port
([browser mesh](/quest/m5/browser-mesh.md)). Browsers in this epic stay
non-transit until it lands.

Decided 2026-10-13: priority then total cost selects among authorized routes;
a direct connection does not always win. A watching tab never re-serves what
it receives. Offloading a room of watchers is a local relay's job
(`moq-cli --p2p` is the small one); warm routes pass only between relays
([cache-aware routing](/quest/m3/cache-routing/README.md)). [Peer route policy](/quest/m4/p2p/cost-scopes.md) sets the cost
knobs and defaults.

Target the public `@moq/net` interface regardless of whether its
[backend](/quest/m5/rs2ts-js.md) is handwritten or built from Rust.
Backend choice does not change the policy that watching tabs never re-serve.

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

Before `@moq/p2p` is first published, add its entrypoints to the API
Extractor declaration checks, which analyze only published packages since
[moq-dev/moq#6794](https://github.com/moq-dev/moq/pull/6794).

## Related

- [E2EE](/quest/m5/e2ee/README.md) - what a peer would need if the token scope stopped being the trust boundary
