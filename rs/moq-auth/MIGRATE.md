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

## 0.1 -> 0.2

### `serve` matches 0.14 relay auth (#4319)

`serve::Policy::revalidate` and `expires` are `Option<Duration>`, defaulting to `None`, so a grant is never re-checked or closed unless you set them.
`serve::Server::new` returns a `Result`: it refuses `revalidate` without `expires` (`Error::UnboundedRevalidate`) and session limits without `revalidate` (`Error::LimitsWithoutRevalidate`).
A session presenting both a JWT and a client certificate is refused with `Refusal::TokenAndCertificate`.
`Key::verify` enforces `nbf` (`Claims::not_before`, failing with `Error::TokenNotYetValid`) and ignores `iss`, `sub`, and `jti`; any other unknown claim still refuses the token.

```rust
// Before
let policy = Policy::default(); // re-checked every 60 s, bounded at 24 h
let server = Server::new(policy);
// After
let mut policy = Policy::default();
policy.revalidate = Some(Duration::from_secs(60));
policy.expires = Some(Duration::from_secs(24 * 60 * 60));
let server = Server::new(policy)?;
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
