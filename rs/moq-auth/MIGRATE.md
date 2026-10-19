# Migrating moq-auth

How to update your code for each breaking change, newest first.

## Unreleased

### `Grant` reads the ambient clock

`Grant::deadline` returns a `moq_clock::Instant` instead of a `tokio::time::Instant`, and no longer needs the `tokio` feature.
Wait on it with `moq_clock::sleep_until`, which runs on tokio's clock in production.
`deadline` and `validate` read wall time from `moq_clock::wall()`, so a test on `moq_clock::sim` or a `moq_clock::Manual` clock decides when a grant expires; stamp `expires` from that clock too.

### Roles carry `Access` (#5697)

`publish` and `subscribe` on `Claims`, `Permissions`, and `Grant` are `Access`, which holds the path patterns and an optional per-connection `Rate` cap.
Read the patterns from `.path`; constructors and `Claims::with_publish` / `with_subscribe` take anything `Into<Access>`, such as a `Pattern`, `Patterns`, or array, so collect a pattern iterator first.
A capped role serializes as `{"path":"alice/**","rate":8000000}`, and key scopes refuse caps.
Released verifiers reject that form, so upgrade every verifier before issuing capped tokens.
A gateway that embeds this crate must enforce a cap or refuse the grant before admitting media.

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
To keep the old 60 s re-check bounded at 24 h, set `revalidate` and `expires` to those values, and handle the `Result` from `Server::new`.

## moq-token 0.7 -> moq-auth 0.1

### moq-token is moq-auth (#3684)

`Claims` holds `Patterns`, and `Claims::authorize` returns pattern residuals.
A `publish` or `subscribe` grant is a pattern: `alice` covers only `alice`, so sign `alice/**` for the subtree.
Replace `moq_token` with `moq_auth` in your imports.
