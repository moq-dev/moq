# moq-token

A simple JWT and JWK based authentication scheme for moq-relay.

For comprehensive documentation including token structure, authorization rules, and examples, see:
**[Authentication Documentation](https://github.com/moq-dev/moq/blob/main/doc/app/relay/auth.md)**

## Quick Usage (symmetric keys)

```bash
# generate secret key
moq-token generate --out key.jwk
# sign a new JWT
moq-token sign --key key.jwk --root demo --publish bbb > token.jwt
# verify the JWT
moq-token verify --key key.jwk < token.jwt
```

## Quick Usage (asymmetric keys)

```bash
# generate private and public keys
moq-token generate --algorithm RS256 --out private.jwk --public public.jwk
# sign a new JWT (using private key)
moq-token sign --key private.jwk --root demo --publish bbb > token.jwt
# verify the JWT (using public key)
moq-token verify --key public.jwk < token.jwt
```
