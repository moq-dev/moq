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

Open, for the implementer to settle and record: the key algorithm (WebCrypto
must support it in every target browser), the identity width (at least 64
bits, so grinding a key to match a targeted identity stays infeasible), and
the exact proof exchange, with replay tests before the wire is fixed.

Tests: a session proving its key gets that identity; two sessions with one key
share it; a restart yields a new one; a session claiming another identity as
its own is refused; a proof copied onto another session is refused; a session
with no proof advertises no route of its own. Wire into CI with Rust/JS
interop.

Wire: `drafts/draft-lcurley-moq-lite.md` gains the key and proof messages in
lite-07-wip; `just drafts check` passes. Public API: likely none beyond
connection setup; report any key handle that leaks into `moq-ffi`.

## Required

- [In-band auth](/quest/m1/auth/README.md) - the AUTH stream carries the key and its proof

## Related

- [Peer grants](/quest/m2/p2p/peer-grant.md) - binds this key as `cnf` for direct sessions
