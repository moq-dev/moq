# @moq/auth

The authorization contract for Media over QUIC, in TypeScript: the request a
relay sends per session and the grant an auth server answers with, plus the JWT
a client presents in its query and the keys that sign and verify it. For the
contract, the relay flags, and worked examples, see the
[Authentication Documentation](../../doc/bin/relay/auth.md).

Grants and claims name paths with patterns from `@moq/pattern`: `foo` is one
broadcast, `foo/**` is a subtree, `**` is everything.

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
import { generate } from "@moq/auth";
// Use this for signing
const key = await generate("HS256");
```

or as a CLI

```bash
# generate secret key
moq-auth generate --key key.jwk
```

The default is HS256, you can choose other algorithms with `--algorithm`:

```bash
moq-auth generate --key key.jwk --algorithm ES256
```

### Signing

You can sign a token as shown below:

```typescript
import { type Claims, load, sign } from "@moq/auth";

const key = load(keyString); // See generate example above
// Create claims
const claims: Claims = {
  root: "demo",
  publish: ["bbb/**"], // `demo/bbb` and everything beneath it
  subscribe: ["**"], // Any broadcast under `demo/`
  exp: Math.floor(Date.now() / 1000) + 3600, // 1 hour from now (in seconds)
  iat: Math.floor(Date.now() / 1000), // Issued at (in seconds)
};

// Sign a token
const token = await sign(key, claims);
```

Or you can sign as a CLI

```bash
moq-auth sign --key "root.jwk" \
  --root "rooms/meeting-123" \
  --subscribe "**" \
  --publish "alice/**" \
  --expires 1703980800 > "alice.jwt"
```

### Verifying

You can also verify a token, then scope it to a connection path the way a relay does:

```typescript
import { authorize, verify } from "@moq/auth";

const claims = await verify(key, token); // signature and expiry
const permissions = authorize(claims, "rooms/meeting-123"); // patterns relative to the path
```

or as a CLI

```bash
moq-auth verify --key root.jwk --root "rooms/meeting-123" < alice.jwt
```

### Answering a relay

A server implementing the contract validates what the relay sends and answers a grant:

```typescript
import { type Grant, GrantSchema, RequestSchema } from "@moq/auth";

const request = RequestSchema.parse(await body.json());
const grant: Grant = GrantSchema.parse({ subscribe: ["**"], expires: 4102444800, revalidate: 60 });
```

### Working example

See **[examples/sign-and-verify.ts](./examples/sign-and-verify.ts)** for a complete working example.

## License

MIT OR Apache-2.0
