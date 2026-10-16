# Migrating moq-auth

How to update your code for each breaking change, newest first.

## Unreleased

### Gateway transports and key sets are non-exhaustive (#3943)

`Transport` gains `Rtmp`, `Srt`, `WebRtc`, and `WebTransport`, and `serve::Keys` gains `Set`; both are `#[non_exhaustive]`, so add a wildcard arm.
A WebTransport session reports `WebTransport` instead of `Quic`, so a policy keyed on `Quic` must accept both.
A JWT verified with one key must carry that key's `kid`; a token without one is accepted only when the key has none.

```rust
match transport {
	Transport::Quic => {}
	_ => {}
}
```

## moq-token 0.7 -> moq-auth 0.1

### moq-token is moq-auth (#3684)

`Claims` holds `Patterns`, and `Claims::authorize` returns pattern residuals.
A `publish` or `subscribe` grant is a pattern: `alice` covers only `alice`, so sign `alice/**` for the subtree.

```rust
// Before
use moq_token::Claims;
// After
use moq_auth::Claims;
```
