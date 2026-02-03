# moq-token

A simple JWT and JWK based authentication scheme for moq-relay.

For comprehensive documentation including token structure, authorization rules, and examples, see:
**[Authentication Documentation](../../doc/concept/authentication.md)**

## Quick Usage (symmetric keys)
```bash
# generate secret key
moq-token --key key.jwk generate
# sign a new JWT
moq-token --key key.jwk sign --root demo --publish bbb > token.jwt
# verify the JWT
moq-token --key key.jwk verify < token.jwt
```

## Quick Usage (asymmetric keys)
```bash
# generate private and public keys
moq-token --key private.jwk generate --algorithm RS256 --public public.jwk
# sign a new JWT (using private key)
moq-token --key private.jwk sign --root demo --publish bbb > token.jwt
# verify the JWT (using public key)
moq-token --key public.jwk verify < token.jwt
```
