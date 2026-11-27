# @moq/auth

The authorization contract for Media over QUIC, in TypeScript: the request a
relay sends per session and the grant an auth server answers with, plus the JWT
a client presents in its query and the keys that sign and verify it. For the
contract, the relay flags, and worked examples, see the
[Authentication Documentation](../../doc/bin/relay/auth.md).

Grants and claims name paths with prefixes from `@moq/net/prefix`: `foo` is `foo`
and everything beneath it, and the empty prefix `""` is everything. Signing takes
no wildcard, so `foo/**`, `**`, and `cam-*` are refused. Tokens, a parsed key's
scope, `Key.public`, and `GrantSchema`'s output spell each prefix as the subtree
pattern releases before 0.22 read, `foo/**` or `**`, and read that spelling back. A role is an `Access`:
its prefixes, or `{"path": [...], "rate": bps}` with a bitrate cap, read and
written as it is.

## Installation

```bash
npm add @moq/auth
```

#### CLI

To use as a CLI (with node installed)

```bash
npm install -g @moq/auth
moq-auth generate ...
```

You can also just directly use it via bun as:

```bash
bunx @moq/auth generate ...
```

## Usage

#### Generation

You would first generate a key as so:

```typescript
import { Key } from "@moq/auth";
// Use this for signing
const key = await Key.generate("HS256");
```

or as a CLI

```bash
# generate secret key
moq-auth generate --out key.jwk
```

The default is HS256, you can choose other algorithms with `--algorithm`:

```bash
moq-auth generate --out key.jwk --algorithm ES256
```

### Signing

You can sign a token as shown below:

```typescript
import { type Claims, Key } from "@moq/auth";

const key = Key.parse(keyString); // See generate example above
// Create claims
const claims: Claims = {
  root: "demo",
  publish: ["bbb"], // `demo/bbb` and everything beneath it
  subscribe: [""], // Any broadcast under `demo/`
  expires: Math.floor(Date.now() / 1000) + 3600, // 1 hour from now (in seconds), written as `exp`
  issued: Math.floor(Date.now() / 1000), // Issued at (in seconds), written as `iat`
};

// Sign a token
const token = await Key.sign(key, claims);
```

Or you can sign as a CLI

```bash
moq-auth sign --key "root.jwk" \
  --root "rooms/meeting-123" \
  --subscribe "" \
  --publish "alice" > "alice.jwt"
```

The token expires in an hour unless `--ttl`, `--expires`, or `--unbounded` says otherwise.

### Verifying

You can also verify a token, then scope it to a connection path the way `moq auth serve` does:

```typescript
import { Claims, Key } from "@moq/auth";

const claims = await Key.verify(key, token); // signature, kid, strict claims, and expiry
const permissions = Claims.authorize(claims, "rooms/meeting-123"); // prefixes relative to the path
```

Or as a CLI:

```bash
moq-auth verify --key root.jwk --in alice.jwt
```

### Answering a relay

A server implementing the contract validates what the relay sends and answers a grant.
A `Grant` is typed as the relay reads it, and the relay refuses a field it does not
know, so check `request.relay` before sending one newer than that relay:

```typescript
import { type Grant, RequestSchema } from "@moq/auth";

const request = RequestSchema.parse(await body.json());
const grant: Grant = { subscribe: { path: [""], rate: 8_000_000 }, expires: 4102444800, revalidate: 60 };
return Response.json(grant);
```

### Working example

See **[examples/sign-and-verify.ts](./examples/sign-and-verify.ts)** for a complete working example.

## License

MIT OR Apache-2.0
