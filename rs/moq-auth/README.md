# moq-auth

The authorization contract for Media over QUIC: the request a relay sends per
session, the grant an auth server answers with, the lease a session holds, the
HTTP client that drives it, and the JWT a client presents in its query.

For the contract, the relay flags, and worked examples, see
**[Authentication Documentation](https://github.com/moq-dev/moq/blob/main/doc/bin/relay/auth.md)**.

Gateway sessions may identify `rtmp`, `srt`, or `webrtc` transport. A lease producer
owns its re-check and expiry clock through `due()`, `update()`, and `failed()`.
`Key::verify` checks signature, algorithm, key ID, strict claims, key scope, expiry, and not-before.

Grants and claims name paths with prefixes: `foo` covers `foo` and everything
beneath it, and the empty prefix is everything. A `Prefix` refuses a wildcard
such as `foo/**` or `*`. Tokens, key scopes, and grants spell each prefix as the
subtree pattern releases before 0.22 read, `foo/**` or `**`, and read that
spelling back as the prefix; any other wildcard there is refused.

## Quick Usage (symmetric keys)

```bash
# generate secret key
moq auth generate --out key.jwk
# sign a new JWT
moq auth sign --key key.jwk --root demo --publish bbb > token.jwt
# verify the JWT
moq auth verify --key key.jwk < token.jwt
```

## Quick Usage (asymmetric keys)

```bash
# generate private and public keys
moq auth generate --algorithm RS256 --out private.jwk --public public.jwk
# sign a new JWT (using private key)
moq auth sign --key private.jwk --root demo --publish bbb > token.jwt
# verify the JWT (using public key)
moq auth verify --key public.jwk < token.jwt
```
