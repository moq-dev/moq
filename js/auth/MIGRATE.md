# Migrating @moq/auth

How to update your code for each breaking change, newest first.

## Unreleased

### Roles carry `Access` (#5697)

Parsed `Claims`, `Grant`, and `authorize`'s permissions hold `publish` and `subscribe` as `Access`: path patterns plus an optional per-connection `rate` cap.
Read the patterns from `.path`. `Key.sign` still takes pattern strings, but a hand-built `Claims` value must come from `ClaimsSchema.parse`.
A gateway must enforce a cap or refuse the grant before admitting media.

```ts
// Before
const allowed = permissions.publish.matches(path);
// After
const allowed = permissions.publish.path.matches(path);
if (permissions.publish.rate !== undefined) throw new Error("bitrate caps are not enforced");
```

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
