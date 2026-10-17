# [M] Signaling and policy

## Goal

An opted-in browser discovers the other opted-in peers on its relay session,
exposes candidates for the application to select, and ends up holding an established
`@moq/net` connection to each selected peer ICE can reach, wired into the shared origin
in both directions. The application supplies the ICE servers and the roster
size above which new negotiations stop using them. No new relay behavior.

## Plan

`Peers` in `@moq/p2p` takes the shared origin, a reconnect-safe handle to the
relay connection's auth, and the policy knobs from the
[questline README](/quest/m2/p2p/README.md): `enabled`, `prefix`,
`iceServers`, `max`, `meta`, plus application-owned selection and per-connection
priority/cost. Decided 2026-10-13: only explicitly selected peers are dialed;
there is no default dial-all. Prefer a selected set/handle to a callback,
settling the public shape with its consumer. The origin carries no auth state
and the pooled `Connection` hides `Established.auth`, yet peer grants need
the identity key proven and AUTH_KEYS/AUTH_PEER received across relay reconnects, so
define that handle before fixing `Peers`. Publishing into the origin reaches
the relay and its announce stream is the roster. Tests inject transport
factories and cover pairing, application selection, and the roster gate.

Identity: the roster id is the origin's route-layer node id, the id its
ROUTE_START carries ([routes](/quest/m1/cluster-routing/routes.md)), owned
once per tab by the origin rather than once per session. lite-07 drops the
hop list, so no hop chain carries it.

Roster: each peer publishes `<prefix><id>` with an `info.json` snapshot track:
the moq ALPNs it accepts, `webrtc: true`, the presenter public key that
[peer grants](/quest/m2/p2p/peer-grant.md) bind, the application's `meta`,
and for native peers an optional WebTransport entry with an ordered list of
URLs and the listener's certificate fingerprint, plus the field
[native pairs](/quest/m2/p2p/native-quic.md) add for ICE+QUIC.
Decided in the quest audit: use a list so a native peer can advertise its
reachable interfaces without choosing one for every browser. This quest
owns the schema shared with [moq-cli](/quest/m2/p2p/cli.md).

Pairing follows explicit selection: pair broadcasts exist only for selected
pairs. The application may still choose a full mesh, so measure that cost
rather than claiming sparsity from the API alone. Incoming offers also require
application admission before a connection is attached to the shared origin;
roster membership and token scope authorize access but do not select peers
on the application's behalf. An application may explicitly accept an offer
without having initiated a dial. When both sides select the same pair and dial at once, the
higher id abandons its own offer on seeing the lower id's, perfect
negotiation with the higher id polite. The dialer publishes
`<prefix><target>/<self>` with a `signal` stream track carrying the offer and
then each ICE candidate as it arrives; the target answers on
`<prefix><self>/<target>`. Every peer subscribes to its own `<prefix><self>/`
prefix.

The gate: a peer connection is built with `iceServers` when the roster holds
`max` peers or fewer, and with an empty list otherwise. Host and mDNS
candidates are always gathered, so LAN pairs form at any roster size.
Established pairs are never closed by the gate. This is a STUN policy, not a
connection budget; test roster churn and pending negotiations separately.

Native entries: when `info.webtransport` is present the dialer tries its
URLs in advertised order, using `connect` with the listener's fingerprint
pinned and the bounded attempt policy, then falls through to WebRTC if none
connects. Mirror this order in the CLI and test that a later address is tried
after an earlier one fails. Chrome 147 may prompt for local network access on that
dial; a denial falls through the same way.

Lifecycle: application deselection or a retracted roster entry closes that
peer's session and releases its routing and signaling demand. Test an
unsolicited offer with no selected/admitted peers, explicit incoming admission,
and deselection during negotiation and while serving. An ICE
failure is terminal for the pair until either side re-announces; no retry
loop. Each established connection goes through `connect` with the supplied
transport on the dialing side and `accept` on the answering side, both with
`publish: origin.consume()` and `consume: origin`, so the tab serves what it
publishes, never what it receives (a tab is not transit).

Trust: publishing under the prefix proves only that the relay admitted the
peer to the prefix, not that it may read everything this tab can. Tokens
within one project carry different subscribe scopes, so a peer session is
scoped like a relay session, but the relay token itself never crosses it: a
`moq_auth` JWT is a bearer credential with no audience or proof of
possession, so a peer that received one could replay it against the relay.
Instead the relay issues each session a peer grant, a relay-signed statement
of that session's path scopes bound to its node id, its presenter public key,
and short-lived, which the peer presents in band with a proof of possession;
the other side verifies the relay's signature, the node id and key against
the roster, and that the key is the one the session proved, then serves only the granted paths. Issuance,
asymmetric keys, JWKS, and refresh live in
[Peer grants](/quest/m2/p2p/peer-grant.md), and the session's key proof in
[Identity proof](/quest/m1/cluster-routing/identity.md): HMAC keys cannot be given to
browsers without also letting them forge grants, so an HS256-only relay
issues nothing. A peer session with no verifiable grant serves nothing;
there is no equal-scope shortcut.

## Required

- [Routes](/quest/m1/cluster-routing/routes.md) - the ROUTE_START node id the roster keys on
- [Peer grants](/quest/m2/p2p/peer-grant.md) - the node-bound, asymmetrically signed credential a direct session presents; HS256 keys issue none
