# [M] Signaling over the relay

## Goal

An opted-in browser discovers the other opted-in peers on its relay session
and ends up holding an established `@moq/net` connection to every one it can
reach on the LAN, wired into the shared origin in both directions. No STUN, no
TURN, no new relay behavior.

## Plan

`Peers` in `@moq/p2p` takes the shared origin and an enabled signal, nothing
else: publishing into the origin reaches the relay, and the origin's announce
stream is the roster. Tests inject a peer connection factory so `bun test`
covers pairing with a fake.

Prefix: `.p2p/` under the session's root path, so the token scope decides who
may join. Identity: a random 64-bit hex id per tab, like the mDNS instance
name. Roster: each peer publishes `.p2p/<id>` with an `info.json` snapshot
track listing the moq ALPNs it accepts, `webrtc: true`, and for native peers
an optional `webtransport: { url, fingerprint }` and `iroh` endpoint id. This
schema is shared with [moq-cli](/quest/m3/p2p/cli.md).

Pairing: the lower id dials. The dialer publishes `.p2p/<target>/<self>` with
a `signal` stream track carrying the offer and then each ICE candidate as it
arrives; the target answers on `.p2p/<self>/<target>`. Every peer subscribes
to its own `.p2p/<self>/` prefix. The peer connection is built with an empty
`iceServers` list; data-channel-only pages get `.local` host candidates from
every browser and resolve the peer's for free.

Native entries: when `info.webtransport` is present the dialer first tries
`Connection.connect` with the fingerprint pinned and a short timeout, then
falls through to WebRTC. Chrome 147 may prompt for local network access on
that dial; a denial falls through the same way.

Lifecycle: a retracted `.p2p/<id>` announce closes that peer's session. An
ICE failure is terminal for the pair until either side re-announces; no
retry loop. Each established connection goes through `Connection.connect`
with the supplied transport on the dialing side and `Connection.accept` on
the answering side, both with `publish: origin.consume()` and
`subscribe: origin`. Because the JS origin serves only what the tab publishes
locally, transit stays a follow-up.

## Required

- [Data channel transport](/quest/m3/p2p/transport.md)

## Related

- [Carrier voice protocol](/quest/m3/carrier-voice/README.md) - the same signaling-as-broadcasts shape
