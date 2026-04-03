# moq-token

A simple JWT-based authentication scheme for moq-relay.

## Quick Usage (symmetric keys)

```bash
# generate secret key
moq-token-cli generate --out key.jwk
# sign a new JWT
moq-token-cli sign --key key.jwk --root demo --publish bbb > token.jwt
# verify the JWT
moq-token-cli verify --key key.jwk < token.jwt
```

## Quick Usage (asymmetric keys)

```bash
# generate private and public keys
moq-token-cli generate --algorithm RS256 --out private.jwk --public public.jwk
# sign a new JWT (using private key)
moq-token-cli sign --key private.jwk --root demo --publish bbb > token.jwt
# verify the JWT (using public key)
moq-token-cli verify --key public.jwk < token.jwt
```
