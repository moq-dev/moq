# [S] moq auth serve bounds what a leaked token or public caller can do

## Goal

`moq auth serve` stops relying on operator discipline for two invariants:

- Admission eventually ends. Today `exp` is never required (`required_spec_claims` is emptied, `moq auth sign` adds none by default) and `Policy.expires` and revalidation are off by default, so a leaked token without `exp` admits forever and admitted sessions never re-check. The server refuses a token without `exp` unless the operator passes an explicit opt-out flag, and `moq auth sign` writes an `exp` by default.
- Only relays drive the session table. Today `POST /` is unauthenticated and `--listen-public` only lifts the loopback bind, so anyone can record sessions with a victim's `remote` (exhausting its `RemoteLimit`) or grow `slots` with random ids. A public listener requires a shared secret or mTLS from relays.

## Plan

Found by the 2026-11-07 invariant audit (auth-3, auth-5).

Decided 2026-11-07 (maintainer, invariant-audit planning): m2, since both change CLI behaviour a deployed operator must adapt to. The opt-out keeps unbounded tokens possible for deployments that revoke by key rotation.
