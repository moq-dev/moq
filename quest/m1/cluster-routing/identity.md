# [M] Identity proof

## Goal

A routing identity is the hash of a per-process (per-tab) key, and every
session, to a relay or direct, proves possession of that key once. No
authenticated session can advertise another origin's identity as its own or
advance its seqno. lite-07 carries the proof; Rust and JS both implement it.

## Plan

Decided 2026-10-14 with the maintainer:

- Each process or tab mints one key at start and derives its routing identity
  by hashing the public key in a dedicated domain. A restart mints a fresh key
  and therefore a fresh identity; every live session of the process shares it,
  so P2P recognizes an origin reached through the CDN and directly.
- A session announces the public key once (AUTH_CNF, moved here from
  [Peer grants](/quest/m2/p2p/peer-grant.md)) and proves possession bound to
  this session: a fresh receiver challenge or the authenticated session
  transcript, never a signature over the key alone, which replays across
  sessions. The receiver then accepts that identity, and only that identity,
  as the session's own. A ROUTE claiming a different identity as its own is a
  protocol violation.
- Identities a session forwards for other origins are not proven by this
  exchange; the route layer files them under `hash(subject, id)` when the
  session's publish grant is scoped
  ([Routes and announces](/quest/m1/cluster-routing/routes.md)).
- Gate lite-07 on it (decided 2026-10-14): ROUTE's safety rests on proven own
  identities. AUTH_KEYS and AUTH_PEER stay with Peer grants and do not gate
  the cut; the peer grant's `cnf` is this key.

The checkpoint below records the candidate algorithm and full-width identity.
The exact authenticated proof exchange remains open, with executable replay
and relay tests before the wire is fixed.

Tests: a session proving its key gets that identity; two sessions with one key
share it; a restart yields a new one; a session claiming another identity as
its own is refused; a proof copied onto another session is refused; a session
with no proof advertises no route of its own. Wire into CI with Rust/JS
interop.

Decided 2026-10-22 (maintainer): this extends the AUTH stream from
[#4039](https://github.com/moq-dev/moq/pull/4039) (`### Auth` in
`drafts/draft-lcurley-moq-lite.md`, `rs/moq-net/src/lite/auth.rs`), adding
AUTH_CNF and the proof to it and specifying their order in the draft, so Rust
and JS implement one stream. It no longer Requires
[Token in band](/quest/m1/auth/token-in-band.md); the auth line stays ranked
ahead of it as a rank, not a block.

Wire: `drafts/draft-lcurley-moq-lite.md` gains the key and proof messages in
lite-07-wip; `just drafts check` passes. Public API: likely none beyond
connection setup; report any key handle that leaks into `moq-ffi`.

Checkpoint 2026-10-22: the draft contains test-only Rust/WebCrypto primitives,
shared signature/identity vectors and an executable mutual keyless-relay
counterexample. The candidate settles routine representation choices as
P-256 ECDSA/SHA-256, canonical 65-byte SEC1 keys, 64-byte P1363 signatures and a
full 256-bit domain-separated identity. It never truncates to legacy Hop.
See [candidate and evidence](/test/interop/identity/README.md).

The goal is not complete. Fresh mutual challenges alone permit live AUTH
relay across two sessions. The fixture demonstrates that acceptance, not an
implemented ROUTE attack. The draft must choose either a common authenticated
transport binding, with explicit unsupported-transport policy and measured
browser/native agreement, or signed ephemeral ECDH plus directional MACs on
all authority-changing routing controls. The latter is viable in WebCrypto
but extends the future ROUTE consumer and wire scope. Chromium 156 and the
existing Node/Bun polyfill expose no exporter; Firefox 157 has method presence
only, and WebKit capability was not established.

After that choice, integrate per-process/tab identity into every session,
reserve the session-wide proof slot before asynchronous crypto, specify and
implement AUTH_CNF/proof ordering in the lite draft and both dispatchers,
preserve independent token refresh/grant checks, enforce own identity and
seqno authority on actual controls, and run live Rust/JS interop. Current
codecs, production crypto dependencies, public API, wire and authentication
behavior are unchanged. The prototype's IDs 2/3 and byte layout are proposals
only. This quest remains active and gates routes until that source and
validation are complete.

## Related

- [Peer grants](/quest/m2/p2p/peer-grant.md) - binds this key as `cnf` for direct sessions
