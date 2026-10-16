# Migrating @moq/auth

How to update your code for each breaking change, newest first.

## Unreleased

### A token must carry its key's `kid` (#3943)

`Key.verify` refuses a token whose `kid` header does not match the key's, and a key set accepts a token without a `kid` only when it holds one key that has none.
The strict transport schema also accepts `rtmp`, `srt`, and `webrtc`, so an exhaustive `switch` over it needs those arms.

```ts
// Sign with the key that will verify it, so the kid matches.
const token = await Key.sign(key, claims);
```

## @moq/token 0.3 -> @moq/auth 0.2

### @moq/token is @moq/auth

`sign` and `verify` are `Key.sign` and `Key.verify`, and claims are pattern unions: `alice` covers only `alice`, so sign `alice/**` for the subtree.

```ts
// Before
import { sign } from "@moq/token";
const token = await sign(key, { put: ["alice"] });
// After
import { Key } from "@moq/auth";
const token = await Key.sign(key, { publish: "alice/**" });
```
