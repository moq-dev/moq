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
settling the public shape with its consumer. Publishing into the origin reaches
the relay and its announce stream is the roster. Tests inject transport
factories and cover pairing, application selection, and the roster gate.

Identity, decided 2026-11-02 (maintainer, /quest-unblock): each peer generates a keypair, and its
roster `<id>` (also the perfect-negotiation tiebreak) is its public key, like an
iroh endpoint id. Nothing proves the key yet: peers are trusted (see Trust).
Which roster peer serves an announce is the application's explicit selection;
moq maps nothing automatically. The route-layer node id stays declared and
unverified ([routes](/quest/m1/cluster-routing/routes/README.md)); if a use case
needs provable identity or signing, moq-lite-07's route identity becomes a
public key, independent of P2P. P2P identity planning and peer grants were
dropped until a real use case asks for them. Switching between a CDN and a
direct path resumes only across a matching explicit epoch.

Roster: each peer publishes `<prefix><id>` with an `info.json` snapshot track:
the moq ALPNs it accepts, `webrtc: true`, the application's `meta`,
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

Trust, decided 2026-11-02 (maintainer, /quest-unblock): P2P trusts its peers. Joining the prefix
takes the relay's token, so a peer admitted to the prefix is as trusted as any
publisher that token admits, and a direct session serves what this tab
publishes without a peer grant or key proof. The relay token itself still
never crosses a peer session: a `moq_auth` JWT is a bearer credential, so a
peer that received one could replay it against the relay. Rejected: relay-issued,
key-bound peer grants with a possession proof (the earlier plan), until a use
case needs per-peer scopes.
