# moq-token

A Javascript/Typescript library and CLI for implementing authentication with a MoQ relay. For comprehensive documentation including token structure, authorization rules, and examples, see the [Authentication Documentation](../../doc/concepts/authentication.md)

## Installation

For general installation as a library

```bash
npm add @moq/token
```


#### CLI

To use as a CLI (with node installed)

```bash
npm install -g @moq/token
```
And then run
```bash
moq-token generate ...
```

You can also just directly use it via bun as:

```bash
bunx @moq/token generate ...
```


## Usage

#### Generation
You would first generate a token as so:

```typescript
import {generate} from "@moq/token";
// Use this for signing
const key = await generate('HS256');
```

or as a CLI
```bash
# generate secret key
moq-token generate --key key.jwk
```

The default is HS256, you can choose other algorithms with `--algorithm`:
```bash
moq-token generate --key key.jwk --algorithm ES256
```

### Signing

You can sign a token as shown below:

```typescript
import { type Claims, load, sign, verify } from "@moq/token";

const key = load(keyString); // See generate example above
// Create claims
const claims: Claims = {
  root: "demo",
  put: "bbb", // Only `demo/bbb`
  get: "", // Any broadcast starting with `demo/`
  exp: Math.floor(Date.now() / 1000) + 3600, // 1 hour from now (in seconds)
  iat: Math.floor(Date.now() / 1000), // Issued at (in seconds)
};

// Sign a token
const token = await sign(key, claims);
```

Or you can sign as a CLI

```bash
moq-token sign --key "root.jwk" \
  --root "rooms/meeting-123" \
  --subscribe "" \
  --publish "alice" \
  --expires 1703980800 > "alice.jwt"
```

### Verifying

You can also verify a token

```typescript
import { verify } from "../src/index.ts";
const rootPath = "rooms/meeting-123";
try{
  const verifiedClaims =<Claims> await verify(key, token, rootPath);
  console.log("Valid token")
} catch(e){
  console.log("Invalid token")
}
```

or as a CLI

```bash
moq-token verify --key root.jwk --root "rooms/meeting-123" < alice.jwt
```

### Working example

See **[examples/sign-and-verify.ts](./examples/sign-and-verify.ts)** for a complete working example.

## API

### Algorithm

Supported algorithms:
- `HS256` - HMAC with SHA-256
- `HS384` - HMAC with SHA-384
- `HS512` - HMAC with SHA-512

### Claims

The JWT payload structure:

```typescript
interface Claims {
	root?: string;           // Root path for publish/subscribe (optional)
	publish?: string;        // Publish permission pattern
	subscribe?: string;      // Subscribe permission pattern
	cluster?: boolean;       // Whether this is a cluster node
	expires?: Date;          // Token expiration time
	issued?: Date;           // Token issued time
}
```

### Key

Key management and JWT operations:

```typescript
interface Key {
	algorithm: Algorithm;
	operations: Operation[];
	secret: string;          // Base64URL encoded secret
	kid?: string;            // Key ID (optional)
}

type Operation = "sign" | "verify" | "decrypt" | "encrypt";

// Load a key from a JWK string
function load(jwk: string): Key;

// Sign claims to create a JWT
function sign(key: Key, claims: Claims): Promise<string>;

// Verify and decode a JWT
function verify(key: Key, token: string): Promise<Claims>;
```

## License

MIT OR Apache-2.0
