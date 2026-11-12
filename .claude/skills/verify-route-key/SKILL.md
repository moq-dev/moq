---
name: verify-route-key
description: Prove lite-07's Route Key Parameter (`rs/moq-net/src/route/proof.rs`, `js/net/src/lite/route_key.ts`) binds a key to its session and side with Tamarin. Use when a change touches what a route key proof signs or how it is checked, a transport binding's `export_keying_material`, TLS early data or resumption, or the draft's Route Key Parameter, or when a replay, reflection, or relaying attack on the proof needs every attacker strategy checked rather than a unit test.
---

# Verify the route key proof

`rs/moq-net/verify/route-key/RouteKey.spthy` models the Route Key Parameter in Tamarin: each endpoint signs the domain, its side, and the session's exported secret, and its peer checks that signature against its own export. The attacker owns the network, makes keys of its own, and runs sessions with every endpoint, including as an intermediary that terminates TLS. Connections are TLS 1.3 full handshakes or resumptions, carrying one raw QUIC session (binding 1) or WebTransport sessions under attacker-chosen IDs (binding 2). Every run records which endpoint is really at its other end, so the lemmas judge a proof against the truth rather than an endpoint's belief.

## Lemmas

Every lemma must verify; the harm says why it matters.

| Lemma | Holds when | Harm if broken |
| --- | --- | --- |
| `executable`, `executable_resumed_webtransport` | Two endpoints can accept each other's keys over raw QUIC, and over WebTransport after resumption. | The other lemmas hold vacuously. |
| `no_relay` | An honest key is never accepted on a session the attacker terminates. | An attacker, or a TLS-terminating intermediary, is filed under an honest relay's key and takes over its routes. |
| `no_reflection` | An endpoint accepts its own key only from itself, as the other side. | The attacker hands a relay its own proof and is filed as that relay. |
| `no_replay` | One signature is accepted on one session only. | A recorded proof claims a key on another connection. |
| `injective_agreement` | An accepted honest key was signed by the session's other end, for that side of that session, and no other session accepts it. | Any of the above. |

## Variants

Each variant drops one assumption the draft states and must break exactly the lemmas its `Breaks:` line names, which keeps the model from passing vacuously.

| Variant | Drops | Breaks |
| --- | --- | --- |
| `NO_SIDE` | The side byte in the signed message. | `no_relay`, `no_reflection`, `injective_agreement` |
| `EARLY` | The main exporter: a resumed client signs over its 0-RTT secret, which a replayed ClientHello reproduces. | `no_replay`, `injective_agreement` |
| `NO_EMS` | A per-session secret: a man in the middle gives its two sessions one, as TLS 1.2 without RFC 7627 allows. | `no_relay`, `no_reflection`, `injective_agreement` |

## Run

```sh
just test route-key-model
just rs tamarin moq-net/verify/route-key/RouteKey.spthy -D=NO_SIDE
```

The first proves the model and checks every variant's `Breaks:` line, in about ten seconds; Nightly runs it. The second proves one variant. To read a counterexample, run `nix shell --inputs-from . nixpkgs#tamarin-prover --command tamarin-prover interactive rs/moq-net/verify/route-key/` and open the falsified lemma.

## Reading a failure

A falsified lemma is an attack path, not yet a bug. Check that each rule it fires matches the code: the transport bindings below for the secret, `proof.rs` for the message. If the code cannot take a step, fix the model. If it can, the signed transcript is wrong: fix it in Rust, JS, and the draft together, and keep `a_proof_matches_the_js_transcript` (Rust) and `route_key.test.ts` (JS) agreeing. Do not add a restriction or drop an attacker rule until a lemma verifies.

## Coverage

Sync commit: `23898b48f46cb54da19007279504b2b6ecdf80cf`. Run `git log 23898b48f4.. -- rs/moq-net/src/route/proof.rs js/net/src/lite/route_key.ts` and the files below to see what changed since. After a change to a covered function, mirror it in the model, rerun `just test route-key-model`, and move the sync commit forward only once the model describes the code at the new commit.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/route/proof.rs` `Transcript::message`, `Transcript::prove`, `DOMAIN`, `EXPORTER_LABEL` (and `sign` in `js/net/src/lite/route_key.ts`) | `Prove` |
| `rs/moq-net/src/route/proof.rs` `Transcript::verify` `rs/moq-net/src/lite/setup.rs` `PeerSetupState::set` | `Accept_from_server`, `Accept_from_client` |
| `rs/moq-net/src/lite/session.rs` `start` (and `proveRouteKey` in `js/net/src/lite/route_key.ts`) | `Session_quic`, `Session_webtransport`, `Prove` |
| `rs/moq-tokio/src/quic/web/session.rs` `webtransport_exporter_context` `rs/moq-uring/src/quic/web.rs` `webtransport_exporter_context` | `Session_webtransport` |
| `rs/moq-quic/src/crypto/rustls.rs` `QuicClientConfig::new`, `QuicServerConfig::new` (TLS 1.3 only, early data off) | `Connect`, `Resume`, and the `EARLY` and `NO_EMS` variants |

## Not covered

Ed25519 is an ideal signature: a small-order public key that verifies anything is a key no honest endpoint holds, which the attacker could share between its own sessions anyway. TLS is reduced to its exporter: a connection's two ends share a fresh secret, resumption mixes the PSK with fresh randoms, and server authentication plays no part, since the proof must hold even when the client dials the attacker. Out of scope: a compromised route key, and relays sharing one key on purpose, which the draft allows. Bindings 3 to 5 (qmux) export nothing, so they never send or rely on a proof. The memory transport and Firefox's WebTransport exporter, which `route_key.ts` skips for deriving the session ID differently, are not modelled.
