# Migrating moq-auth

How to update your code for each breaking change, newest first.

## Unreleased

### RSA uses aws-lc-rs (#6898)

Private RSA JWKs must supply `d`, `p`, `q`, `dp`, `dq`, and `qi`; partial private keys and multi-prime `oth` are refused, including on verify-only imports.
Export a complete two-prime JWK from your key provider or generate one with `moq auth generate --algorithm RS256`.
Existing complete JWKs, public keys, and tokens keep their format; generated RSA keys remain 2048 bits.
`Error` no longer implements `From<rsa::Error>` or `From<rsa::pkcs1::Error>`: handle those dependency errors in your own caller instead of converting them through moq-auth.

### Tiers are metric-safe names (#6778)

A tier is `/`-joined segments of ASCII letters, digits, `.`, `_`, and `-`, checked by `tier::validate`; the empty tier is the default.
`Grant::validate` refuses any other with `Error::InvalidTier`, a `Refusal` naming one does not deserialize, and `serve::Server::new` refuses a `Policy::tier` (`moq auth serve --tier`) outside the rule.
Rename tiers that carry spaces, quotes, or other punctuation, for example `gold tier` to `gold-tier`.

### `serve::Refusal` is `serve::Denial` (#6510)

The reference server's policy failure is `serve::Denial`, so `Refusal` names only the shared 403 body, `moq_auth::Refusal`.
Rename `serve::Refusal` to `serve::Denial`; its variants are unchanged, and `Policy::decide` and `Server::answer` return it.

### `Key` and `KeySet` parse through `FromStr` (#6471)

The inherent `Key::from_str` and `KeySet::from_str` are replaced by `impl FromStr`, with the same `Error`.
Call `text.parse::<Key>()`, or bring `std::str::FromStr` into scope to keep calling `Key::from_str`.

### `Client::connect` has no timeout of its own (#5765)

`Client::connect` waits on the auth server for as long as its caller does, and dropping the future drops the request.
A caller without its own deadline, such as a handshake deadline, wraps the call in one, for example `tokio::time::timeout(Duration::from_secs(10), client.connect(request))`.
`attach`, revalidate, and end keep their ten-second bound and still report a stalled server as `Error::Unavailable`.

### Typed admission refusals (#5996)

`Error::Refused` now carries a `Refusal`; construct `Error::Refused(Refusal::default())` for an unattributed refusal and match `Error::Refused(refusal)` to read `reason`, `root`, and `tier`.
HTTP auth servers can return that JSON shape with 403; legacy plain-text 401/403 still refuses without attribution.
`serve::Refusal::{NoAccess,TokenLimit,RemoteLimit}` now carry an optional verified `root`, and `Expired` carries the verified root.

### `Grant` reads the ambient clock

`Grant::deadline` returns a `kio::clock::Instant` instead of a `tokio::time::Instant`, and no longer needs the `tokio` feature.
Wait on it with `kio::clock::sleep_until`, which runs on tokio's clock in production.
`deadline` and `validate` read wall time from `kio::clock::wall()`, so a test on `kio::clock::sim` or a `kio::clock::Manual` clock decides when a grant expires; stamp `expires` from that clock too.

`Key::verify` and the reference server's default expiry use that wall clock too.
Session `End` durations and the server's idle-session accounting use its monotonic clock, so advancing controlled time advances those durations without a host-clock wait.

### Auth roles are patterns

`Claims`, `Grant`, and `Permissions` hold `publish` and `subscribe` as `Patterns`, matching release.
Remove the `Access` wrapper and read the role directly, for example `permissions.publish.matches(path)` instead of `permissions.publish.path.matches(path)`.
`Claims::with_publish` and `with_subscribe` take pattern iterators again.
`Access`, `Rate`, and `InvalidRate` are removed; bitrate restrictions are unsupported and the whole object role is refused, including `{"path":["alice/**"],"rate":8000000}`.
Uncapped token and grant encodings are unchanged. Use pattern arrays on the wire; the unpublished bare-string and `{path}` role shortcuts are removed too.

`Key::decode` is removed. Call `Key::verify` for MoQ tokens so signature, algorithm, key ID, strict claims, key scope, expiry, and not-before are all checked.
Use a JWT library directly for a different application's payload and validation policy.

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
