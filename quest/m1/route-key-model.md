# [M] A symbolic model proves a route key proof binds to its session and side

## Goal

A Tamarin model of lite-07's Route Key Parameter proves that when an endpoint
accepts a key on a session, the holder of that key signed for that session
and that side of it. The attacker controls the network, runs its own sessions
with both endpoints, and holds other route keys. The draft states what each
transport binding's exporter must guarantee for the proof to hold, each
binding in Rust and JS is checked against that, and the model runs in Nightly.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net):

- ✅ Tamarin (rejected: no formal analysis; the proof is a hand-rolled
  construction, the kind symbolic provers find reflection and relaying
  attacks in).
- ✅ m1 and it gates the cut (rejected: ungated like the TLA+ models; m4).
  The signed transcript freezes when lite-07 is cut, and a flaw in it can
  only be fixed on the wire, so
  [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) lists this quest.

The proof (`rs/moq-net/src/route/proof.rs`, `js/net/src/lite/`, the draft's
Route Key Parameter section) is an Ed25519 signature over the domain string,
one byte for the sender's side, and 32 bytes exported from the session.

- Model the handshake as a session whose exporter secret only its two
  endpoints know and no other session shares. That assumption is the result
  to pin down: state it in the draft, then check each binding against it.
  Cover at least TLS 1.3 only (an older TLS without extended master secret
  lets one party give two sessions the same secret), resumption and 0-RTT, the
  WebTransport session exporter, and a session reached through an intermediary
  that terminates TLS.
- Lemmas: the protocol can run; an accepted proof implies its key's holder
  signed on that session as that side (injective agreement); no reflection to
  the sender; no replay on another session; a proof the attacker relays
  between two sessions it terminates is rejected.
- Out of scope: an endpoint whose own key is compromised, and relays that
  share one key on purpose, which the draft allows.
- A lemma that fails is a finding: fix the transcript in Rust, JS, and the
  draft in this PR when the fix is clear, and bring the maintainer the wire
  change when it is not.
- Keep the model beside the others (`rs/moq-net/verify/`), with a `verify-*`
  skill in their shape and Tamarin run from the flake as the `tla` recipe
  runs TLC.

Public API: none unless a finding changes the proof. Wire: none unless a
finding changes the signed transcript.

## Related

- [The TLA+ models run nightly and reviews resync them](/quest/m1/model-checks-nightly.md) - the Nightly job and Coverage-table check this model joins
- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - files a peer's routes under the key this proves
