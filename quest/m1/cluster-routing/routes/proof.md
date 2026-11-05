# [M] Scoped peers prove a route key

## Goal

A lite-07 peer proves a route key once per session in SETUP. A relay files
every identity a scoped, proven peer sends, its own included, under
H(domain, proven key, id). Relays sharing a key file fail over as one;
unscoped peers stay bare and unproven scoped peers get a per-session key.
Proof production and verification are implemented by this PR; filing belongs
to [Route session integration](/quest/m1/cluster-routing/routes/session.md).

## Plan

✅ Decided 2026-11-05 (maintainer, /quest-plan): prove the route key once per
session in lite-07 SETUP parameter 0x8, without a nonce, and keep the node's
own identity a random u64. This supersedes the 2026-11-04 bullets requiring
proof per announce stream, a nonce in ANNOUNCE_REQUEST, a proof in
ANNOUNCE_OK, and H(key, per-process salt) for the own identity. The signature
already covers TLS exporter output unique to the session, so a nonce adds
nothing. A session has one key, so proving it once in SETUP costs less than
per announce stream. A peer cannot verify H(key, per-process salt), making
it equivalent to random. ROUTE retains its u64 identity.

The other five choices in the PR remain as made:

- The own identity stays a fresh random u64, as decided above.
- moq-net calls rind directly; moq-crypto's sign feature depends on moq-net,
  so moving one key type into another crate adds unnecessary machinery.
- The key lives on origin::Config. The relay's --cluster-route-key file
  covers accepted and dialed sessions; moq-tokio needs no additional flag.
- qmux currently provides no exporter and proves nothing. This is an
  implementation limitation, not a prohibition on TLS bindings.
- Browsers prove a per-page WebCrypto key when their transport exposes
  exportKeyingMaterial, without adding a public JS API.

The 2026-11-04 choices to ship before the lite-07 cut, share a relay key
file, qualify the proven peer's own identity, and retain u64 ROUTE identities
stand. A missing key file is generated; without a configured file, a relay
generates a fresh key on startup.

The signed transcript is ASCII `moq-lite route key`, one role byte (0 client,
1 server), and 32 bytes from exporter label `EXPORTER-moq-lite-route-key`
with empty context. The proof is the 32-byte public key plus 64-byte
signature. No ANNOUNCE_OK proof body is needed by this draft or implementation;
[lite-07 route wire](https://github.com/moq-dev/moq/pull/7764) owns its message changes.

## Related

- [Route session integration](/quest/m1/cluster-routing/routes/session.md) - files identities under the proven key
