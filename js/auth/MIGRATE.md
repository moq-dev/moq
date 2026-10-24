# Migrating @moq/auth

How to update your code for each breaking change, newest first.

## Unreleased

### Auth roles are patterns

Parsed `Claims` and `Grant` hold `publish` and `subscribe` as pattern string arrays, matching release; `authorize` returns `Patterns` in each role.
Remove `Access` and `AccessSchema`; pass pattern arrays to claims and grants and call `permissions.publish.matches(path)` directly instead of using `.path`.
Bitrate restrictions are unsupported and the whole object role is refused, including `{"path":["alice/**"],"rate":8000000}`.
Uncapped token and grant encodings are unchanged. Use pattern arrays on the wire; the unpublished bare-string and `{path}` role shortcuts are removed too.

`Key.decode` is removed. Use `Key.verify(key, token)` for MoQ tokens so signature, algorithm, key ID, strict claims, key scope, expiry, and not-before are all checked.
Use a JWT library directly for a different application's payload and validation policy.

### A token must carry its key's `kid` (#3943)

`Key.verify` refuses a token whose `kid` header does not match the key's, and a key set accepts a token without a `kid` only when it holds one key that has none.
Sign with the key that will verify it, so the `kid` matches.
The strict transport schema also accepts `rtmp`, `srt`, `webrtc`, and `webtransport`, so an exhaustive `switch` over it needs those arms.
A WebTransport session reports `webtransport` instead of `quic` (#5293), so a policy keyed on `quic` must accept both.

## @moq/token 0.3 -> @moq/auth 0.2

### @moq/token is @moq/auth

`sign` and `verify` are `Key.sign` and `Key.verify`, and claims are pattern unions: `alice` covers only `alice`, so sign `alice/**` for the subtree.

```ts
import { Key } from "@moq/auth";
const token = await Key.sign(key, { publish: "alice/**" });
```
