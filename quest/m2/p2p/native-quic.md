# [L] Native pairs over ICE and QUIC

## Goal

Two native peers (`moq-cli --p2p`, or any `moq-tokio` client that opts in)
selected by their applications connect directly across a NAT: ICE finds a
path through host and server-reflexive candidates, then a `moq-quic`
connection runs over the nominated pair, with each side's certificate pinned
by the fingerprint the other sent in its offer or answer. A pair with a
browser still uses a [data channel](/quest/m2/p2p/webrtc.md).

## Plan

Decided with the maintainer on 2026-10-13:

- No iroh. It brings its own discovery and relaying beside the relay, which
  this line rejects; native pairs use the same STUN and ICE as browsers, with
  the relay as rendezvous and fallback.
- ICE is str0m's standalone sans-IO agent, the `is` crate (0.11, already in
  the lockfile through str0m; str0m re-exports it as `str0m::ice`). Gather
  candidates through the module [webrtc](/quest/m2/p2p/webrtc.md) lifts out
  of `moq-rtc`, with host candidates, `.local` resolution, and the STUN
  Binding client; don't copy it.
- QUIC is `moq-quic`, fed datagrams from the nominated pair.
- Authentication: each side generates a self-signed certificate and sends
  its SHA-256 fingerprint in its offer or answer, as DTLS fingerprints ride
  in SDP. Both sides pin it with the existing `tls.fingerprint` check. MoQ
  AUTH with a [peer grant](/quest/m2/p2p/peer-grant.md) still runs on top.
- After nomination the session's socket carries ICE consent checks and
  QUIC, split by the [UDP demux](/quest/m2/p2p/udp-demux.md) classifier.

Roles are deterministic: the ICE controlling side is the QUIC client, so a
simultaneous dial resolves the same way [signaling](/quest/m2/p2p/signal.md)
resolves offers (the lower id wins). The offer and answer carry ICE
credentials, the fingerprint, and candidates on the pair's `signal` track;
settle the field names with signaling's schema, and add a roster field
saying a native peer accepts ICE+QUIC so a native dialer picks this over a
data channel. A failed ICE check is terminal for the pair, like signaling's
rule; no retry loop. The QUIC connection's path never migrates; ICE
restarts are out of scope.

Open question for the implementer: `is` may re-nominate a different pair
without an ICE restart (a late trickled candidate, a network change), while
QUIC stays on the old tuple. Either freeze nomination once the QUIC
handshake starts, keeping consent checks on that pair, or follow the new
pair by migrating or recreating the QUIC path. Recommendation: freeze, and
if `is` cannot hold a pair, close the session on a nomination change rather
than migrate; it matches the no-retry rule and keeps QUIC on one path.
Either way, a test trickles a better candidate after QUIC starts and shows
the session never stalls on a stale tuple.

Ordered qmux stays the only data channel mode: native pairs have
independent QUIC streams here, so unordered reassembly would serve nobody.

Tests: unit tests drive two agents and two `moq-quic` endpoints in memory
with mocked time, covering nomination, a wrong fingerprint refused, and a
consent-check failure closing the session. The [harness](/quest/m2/p2p/harness.md)
covers LAN and NAT pairs between two `moq-cli --p2p` nodes.

## Required

- [UDP demux](/quest/m2/p2p/udp-demux.md) - the classifier that splits STUN from QUIC
- [Native data channel transport](/quest/m2/p2p/webrtc.md) - the shared ICE gathering module
- [Signaling and policy](/quest/m2/p2p/signal.md) - the offer schema and roster this extends
