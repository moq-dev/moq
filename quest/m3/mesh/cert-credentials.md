# [M] Certificate credentials

## Goal

A client can present an X.509 certificate chain over its Ed25519 key in place
of a JWT: in the `jwt` query, as a moq-transport SETUP token, and on every
transport, browsers included. The credential is the chain plus a proof
signature over the relay's host name, the dialed path, and the current time,
so a captured chain alone admits nothing. `moq-auth` parses and verifies it
against a trust store the embedding application supplies and swaps at
runtime: its anchors, and a check for whether a certificate is revoked. A
relay or node using that store refuses a chain that fails it, and closes
sessions it admitted earlier once their certificate is revoked or the anchors
change, at its next re-check. The application turns a verified chain into a
grant, the way in-process admissions already decide. `moq_auth::mesh::Client`
can present such a credential, and `@moq/auth` builds one from a
non-extractable WebCrypto Ed25519 key.

## Plan

Decided 2026-11-07 (maintainer, Kixel's `/quest-plan`): a mesh whose devices
must admit and evict each other with no internet needs credentials that last
longer than a coordinator token and can be checked offline, so Kixel moved its
devices from coordinator-minted JWTs to 30-day certificates from the mesh's
own signers. Presenting the certificate as the token, rather than over mTLS,
gives one path that also crosses relays that accept only their own cluster's
client certificates, WebSocket fallbacks, and browsers, which cannot present
client certificates over WebTransport. The signer log, revocation set, and
their sync stay in Kixel; this quest is the generic format and hook.

- Verify chains with webpki. Name constraints work only on dNSName (webpki
  rejects URI constraints and lacks directoryName), so document scopes
  carried as DNS names.
- The trust store is a handle the application updates, not a callback per
  connection. Re-check live sessions on the relay's existing `--revalidate`
  cadence.
- The time-window proof comes first. Its replay window (a relay that saw a
  credential could reuse it at the same host within the skew) closes with the
  [challenge proof](/quest/m4/cert-challenge.md). Settle the skew and the
  signed encoding before coding, and record them in `doc/bin/relay/auth.md`.
- Browsers: WebCrypto Ed25519 keys sign in current Chrome, Firefox, and
  Safari; confirm it before relying on it, and fail loud where it is missing.
- Overlaps [moq-dev/moq#8149](https://github.com/moq-dev/moq/pull/8149)'s
  bounded revocation for `moq auth`'s own mesh coordinator, which keeps
  JWTs; this quest does not change that coordinator's tokens.

Tests: a valid chain admits; a revoked or unanchored one, a proof for another
host or path, and a stale proof are refused; a session admitted before its
certificate is revoked closes on the next re-check; swapping the anchors
applies without a restart; a JS client presents a credential from a WebCrypto
key and a Rust relay admits it.

Public API: new credential types in `moq-auth` and `@moq/auth`, and a trust
store handle on the relay. Wire: a new credential form wherever a JWT is
accepted.

Added 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests), before any code: pin the credential encoding, since it is permanent once shipped.

- A self-describing, versioned encoding (for example a `moqc1.` prefix, or a distinct token type) in a slot named by role. If the query key stays `jwt` for compatibility, the prefix still makes it dispatchable without sniffing, and moq-transport uses its AUTHORIZATION TOKEN type field.
- One proof structure whose signed payload has a `nonce | timestamp` field, so [the challenge](/quest/m4/cert-challenge.md) reuses it instead of adding a second format.
- Revocation is data in the trust-store handle (a revoked SPKI-hash set, or a set of serials qualified by their issuer, which the application replaces), not a per-connection predicate. A bare serial is never enough, since two trusted issuers can reuse one; a test revokes one issuer's serial and still admits the other issuer's certificate with the same serial.
- Chains are 2 to 4 KB in base64, close to common 8 KB URL limits; cache verified chains by fingerprint, and say in the PR how a long chain is carried.
- The shared token corpus planned in [moq-dev/moq#8151](https://github.com/moq-dev/moq/pull/8151) includes this credential form.

## Related

- [Mesh relay node](/quest/m3/mesh/node.md) - checks these credentials for its LAN peers, and only uses them, so this lands first
