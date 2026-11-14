# [S] A malleated signature does not double a token's session cap

## Goal

The auth server counts sessions per token by the token's signing input
(header and payload), not by the whole compact JWS. Today the per-token count
hashes the full string (`Sessions::connect` in `rs/moq-auth/src/serve.rs`),
and an ECDSA signature can be malleated into a second valid string for the
same claims, so an ES256 or ES384 holder gets twice `--limit-token`.

## Plan

Found by the 2026-11-07 invariant audit (auth-6). Split 2026-11-12
(maintainer, /quest-audit before the cut) from
[the O(1) session index](/quest/m4/auth-session-index.md): ✅ this is a fix
to shipped behavior, since token caps are in `release`, so it joins m1 and
can be backported; the index stays an optimization in m4 (rejected: leaving
the fix inside the optimization).

- Key the count on the signing input or the decoded claims; pick whichever
  the verifier already has in hand, and say which in the PR.
- Reproduce first: a test that connects with a token and its malleated twin
  and exceeds the cap on `main`.
- Check the other places a token is used as a map key (attachment leases,
  shared request-token grants) for the same assumption, and fix each in this
  PR.

Public API: none. Wire: none.
