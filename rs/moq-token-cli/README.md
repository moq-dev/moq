# moq-token

A simple JWT-based authentication scheme for moq-relay.

## Quick Usage (symmetric keys)
```bash
# generate secret key
moq-token-cli --key key.jwk generate
# sign a new JWT
moq-token-cli --key key.jwk sign --root demo --publish bbb > token.jwt
# verify the JWT
moq-token-cli --key key.jwk verify < token.jwt
```

## Quick Usage (asymmetric keys)
```bash
# generate private and public keys
moq-token-cli --key private.jwk generate --algorithm RS256 --public public.jwk
# sign a new JWT (using private key)
moq-token-cli --key private.jwk sign --root demo --publish bbb > token.jwt
# verify the JWT (using public key)
moq-token-cli --key public.jwk verify < token.jwt
```
