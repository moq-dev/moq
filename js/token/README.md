# moq-token

A simple JWT and JWK based authentication scheme for moq-relay.

## Installation

```bash
npm add moq-token
```

## Usage

See **[examples/sign-and-verify.ts](examples/sign-and-verify.ts)** for a complete working example.

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
