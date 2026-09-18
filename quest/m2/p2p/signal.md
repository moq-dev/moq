# [M] Signaling and policy

## Goal

An opted-in browser discovers the other opted-in peers on its relay session,
asks the application which to dial, and ends up holding an established
`@moq/net` connection to each one ICE can reach, wired into the shared origin
in both directions. The application supplies the ICE servers and the roster
size above which new negotiations stop using them. No new relay behavior.

## Plan

`Peers` in `@moq/p2p` takes the shared origin and the policy knobs from the
[questline README](/quest/m2/p2p/README.md): `enabled`, `prefix`,
`iceServers`, `max`, `meta`, `select`. Publishing into the origin reaches the
relay and the origin's announce stream is the roster. Tests inject a peer
connection factory so `bun test` covers pairing, the roster gate, and
`select` with a fake.

Identity: the roster id is the origin's hop id. The handshake already
declares a random hop per session; move ownership of that value to the
origin, one per tab, as `origin::Producer::empty(Hop::random())` does in
Rust, so the id in the roster is the id in every chain the tab forwards.

Roster: each peer publishes `<prefix><id>` with an `info.json` snapshot track:
the moq ALPNs it accepts, `webrtc: true`, the application's `meta`, and for
native peers an optional `webtransport: { url, fingerprint }` and `iroh`
endpoint id. The schema is shared with [moq-cli](/quest/m2/p2p/cli.md).

Pairing is sparse: broadcasts exist only for pairs `select` chose, so the
cost is the dialed pairs, not the square of the roster. The lower id dials.
The dialer publishes `<prefix><target>/<self>` with a `signal` stream track
carrying the offer and then each ICE candidate as it arrives; the target
answers on `<prefix><self>/<target>`. Every peer subscribes to its own
`<prefix><self>/` prefix.

The gate: a peer connection is built with `iceServers` when the roster holds
`max` peers or fewer, and with an empty list otherwise. Host and mDNS
candidates are always gathered, so LAN pairs form at any roster size.
Established pairs are never closed by the gate.

Native entries: when `info.webtransport` is present the dialer first tries
`connect` with the fingerprint pinned and a short timeout, then falls
through to WebRTC. Chrome 147 may prompt for local network access on that
dial; a denial falls through the same way.

Lifecycle: a retracted roster entry closes that peer's session. An ICE
failure is terminal for the pair until either side re-announces; no retry
loop. Each established connection goes through `connect` with the supplied
transport on the dialing side and `accept` on the answering side, both with
`publish: origin.consume()` and `consume: origin`, so the tab serves what it
publishes and, once [transit](/quest/m2/p2p/transit.md) lands, what it
receives.

## Required

- [Data channel transport](/quest/m2/p2p/transport.md)

## Related

- [Carrier voice protocol](/quest/m3/carrier-voice/README.md) - the same signaling-as-broadcasts shape
