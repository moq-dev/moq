# Web P2P

## Goal

A measured verdict on whether moq-lite over WebRTC data channels is good enough
for browsers on one LAN to serve each other, so a room full of watchers costs
the relay one egress stream instead of one per tab. Opt-in and LAN only: ICE
gathers host candidates and nothing else, never STUN or TURN, so a connection
can only form between peers that already share a network.

Two endpoint pairs must work: browser to browser, and browser to a native node
on the LAN. A browser reaches a native node over WebTransport when it can and
over a data channel when it cannot. Native nodes keep preferring QUIC among
themselves, over the existing mDNS mesh or an advertised iroh endpoint.

Non-goals: NAT traversal, multi-hop browser meshes, RTP media, and operating
without a relay. Offline LAN operation remains the native mesh's job.

## Plan

### The relay is the rendezvous

No browser exposes mDNS or DNS-SD to a page, and Chrome's Local Network Access
work moves the other way. But every opted-in peer already holds a session to
the same relay, so discovery and signaling ride that session as ordinary moq
broadcasts under a reserved `.p2p/` prefix, the same shape
[carrier voice](/quest/m3/carrier-voice/README.md) uses for call setup. The
relay learns nothing new. Trust is the relay's token scope: a peer that may
publish under `.p2p/` is as trusted as any other publisher the token admits,
so the prototype needs no E2EE.

LAN scoping needs no code either. With an empty ICE server list a browser
offers host candidates only, obfuscated as `.local` mDNS names it resolves for
its peers. The native side must resolve those names itself.

### Two stream mappings, both measured

Data channels are SCTP streams, so moq-lite streams can map one-to-one onto
channels, or qmux can multiplex everything over a single channel the way the
WebSocket binding does. The first keeps groups independent but fights Chrome's
1024-stream cap and 256 KiB message limit; the second is nearly free but
reintroduces head-of-line blocking across groups. Both ship in the browser and
native transports and the verdict compares them against WebTransport on the
same LAN, in Chrome and Firefox, whose SCTP stacks differ.

### What the browser cannot do yet

The `@moq/net` origin never forwards what a peer announced, so a watcher tab
cannot re-serve a broadcast it receives from the relay. The prototype measures
the two topologies that work without transit: a publishing tab serving LAN
watchers directly, and watcher tabs pulling from a `moq-cli` hop that already
transits. Route selection between the relay and a LAN peer is
[cost ranking in the JS origin](/quest/m2/route-cost.md), which is product
work and lives in m2. A go verdict promotes this line to m2 with a JS transit
quest and the productization work; a no-go is a written abandonment.

### Risks the verdict records

- Chrome 147 prompts for local network access on a WebTransport dial to a
  private address from a public origin; the same gate is proposed for WebRTC.
  The fallback order covers a denial, but join time suffers.
- Firefox bug 1698141: Firefox-initiated LAN P2P to Chrome can fail on mDNS
  candidate parsing.
- Networks that filter multicast (client isolation, VLAN hops) break `.local`
  resolution and therefore every browser connection.
- Chrome caps an association at 1024 SCTP streams with 256 KiB messages and
  interleaves fragments only behind a flag, so the per-stream mapping's numbers
  are browser-specific.

## Quests

- [Data channel transport](/quest/m3/p2p/transport.md) - `@moq/p2p` speaks moq-lite over an RTCPeerConnection in both mappings, behind the WebTransport shape `@moq/net` already consumes
- [Signaling over the relay](/quest/m3/p2p/signal.md) - opted-in peers find each other under `.p2p/` and end up with established sessions on the shared origin
- [Native data channel transport](/quest/m3/p2p/webrtc.md) - `moq-tokio` holds a moq-net session with a browser over str0m, resolving `.local` candidates
- [moq-cli joins the LAN](/quest/m3/p2p/cli.md) - `--p2p` advertises WebTransport, iroh, and WebRTC reachability and accepts browsers as a transit hop
- [Watch opts in](/quest/m3/p2p/watch.md) - one attribute turns it on in the demo and the watcher picks the cheapest route
- [Verdict](/quest/m3/p2p/verdict.md) - the Playwright harness, the numbers, and the go/no-go

## Related

- [Route cost in the JS origin](/quest/m2/route-cost.md) - the watcher-side route pick this line needs
- [E2EE](/quest/m2/e2ee/README.md) - what a LAN peer would need if the token scope stopped being the trust boundary
- [Cluster discovery flags](/quest/m2/cluster-flags.md) - the flag shape `--p2p` should follow
- [qmux on the QUIC core](/quest/m2/quic/qmux.md) - the qmux mapping rides whatever stream core that line selects
- [Carrier voice](/quest/m3/carrier-voice/README.md) - signaling as an application protocol over moq, the pattern reused here
