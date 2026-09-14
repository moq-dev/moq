# moq-auth

The authorization contract for Media over QUIC: the request a relay sends per
session, the grant an auth server answers with, the lease a session holds, the
HTTP client that drives it, and the JWT a client presents in its query.

For the contract, the relay flags, and worked examples, see
**[Authentication Documentation](https://github.com/moq-dev/moq/blob/main/doc/bin/relay/auth.md)**.

Grants and claims name paths with patterns: `foo` is one broadcast, `foo/**` is
a subtree, `**` is everything.

## Quick Usage (symmetric keys)

```bash
# generate secret key
moq auth generate --out key.jwk
# sign a new JWT
moq auth sign --key key.jwk --root demo --publish 'bbb/**' > token.jwt
# verify the JWT
moq auth verify --key key.jwk < token.jwt
```

## Quick Usage (asymmetric keys)

```bash
# generate private and public keys
moq auth generate --algorithm RS256 --out private.jwk --public public.jwk
# sign a new JWT (using private key)
moq auth sign --key private.jwk --root demo --publish 'bbb/**' > token.jwt
# verify the JWT (using public key)
moq auth verify --key public.jwk < token.jwt
```
