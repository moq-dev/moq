# [M] Signaling and policy

## Goal

An opted-in browser discovers the other opted-in peers on its relay session,
asks the application which to dial, and ends up holding an established
`@moq/net` connection to each one ICE can reach, wired into the shared origin
in both directions. The application supplies the ICE servers and the roster
size above which new negotiations stop using them. No new relay behavior.

## Plan

`Peers` in `@moq/p2p` takes the shared origin and the policy knobs from the
[questline README](/quest/next/p2p/README.md): `enabled`, `prefix`,
`iceServers`, `max`, `meta`, `select`. Publishing into the origin reaches the
relay and the origin's announce stream is the roster. Tests inject a peer
connection factory so `bun test` covers pairing, the roster gate, and
`select` with a fake.

Identity: the roster id is the origin's hop id. The handshake already
declares a random hop per session; move ownership of that value to the
origin, one per tab, as `origin::Producer::empty(Hop::random())` does in
Rust, so the id in the roster is the id in every chain the tab forwards.

Roster: each peer publishes `<prefix><id>` with an `info.json` snapshot track:
the moq ALPNs it accepts, `webrtc: true`, whether it can run qmux unordered,
the presenter public key that
[peer grants](/quest/next/auth/peer-grant.md) bind, the application's `meta`,
and for native peers an optional `webtransport: { url, fingerprint }` and
`iroh` endpoint id. The schema is shared with
[moq-cli](/quest/next/p2p/cli.md). Unordered is advertised here so the dialer
can set `RTCDataChannel.ordered` at create time; see
[unordered qmux](/quest/next/p2p/unordered.md).

Pairing is sparse: broadcasts exist only for pairs some `select` chose, so
the cost is the dialed pairs, not the square of the roster. Whoever selects a
pair dials it; `select` decides initiative, not admission, and an answerer
accepts any offer from a roster peer because the token scope already decided
who may be there. When both sides select the same pair and dial at once, the
higher id abandons its own offer on seeing the lower id's, perfect
negotiation with the higher id polite. The dialer publishes
`<prefix><target>/<self>` with a `signal` stream track carrying the offer and
then each ICE candidate as it arrives; the target answers on
`<prefix><self>/<target>`. Every peer subscribes to its own `<prefix><self>/`
prefix.

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
publishes and, once [transit](/quest/next/p2p/transit.md) lands, what it
receives.

Trust: publishing under the prefix proves only that the relay admitted the
peer to the prefix, not that it may read everything this tab can. Tokens
within one project carry different subscribe scopes, so a peer session is
scoped like a relay session, but the relay token itself never crosses it: a
`moq_auth` JWT is a bearer credential with no audience or proof of
possession, so a peer that received one could replay it against the relay.
Instead the relay issues each session a peer grant, a relay-signed statement
of that session's path scopes bound to its hop id, its presenter public key,
and short-lived, which the peer presents in band with a proof of possession;
the other side verifies the relay's signature, the hop id and key against
the roster, and AUTH_POP, then serves only the granted paths. Issuance,
asymmetric keys, JWKS, PoP, and refresh live in
[Peer grants](/quest/next/auth/peer-grant.md): HMAC keys cannot be given to
browsers without also letting them forge grants, so an HS256-only relay
issues nothing. A peer session with no verifiable grant serves nothing;
there is no equal-scope shortcut.

## Required

- [Data channel transport](/quest/next/p2p/transport.md)
- [Peer grants](/quest/next/auth/peer-grant.md) - the hop-bound, asymmetrically signed credential a direct session presents; HS256 keys issue none

## Related

- [Carrier voice protocol](/quest/future/carrier-voice/README.md) - the same signaling-as-broadcasts shape
