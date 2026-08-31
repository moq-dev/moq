# [XS] Avoid allocation in axum/tungstenite WebSocket message conversion

## Goal

Implement and verify the behavior tracked in [#1095](https://github.com/moq-dev/moq/issues/1095)
within the issue's stated scope and boundaries.

## Plan

Still real on main, and now trivial: the lockfile resolves a single
tungstenite 0.29 and both Binary variants wrap bytes::Bytes, so the
conversion can be zero-copy without a version dance.

### Issue context

#### Problem

In `rs/moq-relay/src/websocket.rs`, converting between axum and tungstenite WebSocket messages requires going through `Vec<u8>` / `String`, which copies the underlying `Bytes` buffer:

```rust
tungstenite::Message::Binary(bin) => axum::extract::ws::Message::Binary(Vec::from(bin).into()),
```

This is because axum bundles its own version of tungstenite internally, so the `Bytes`/`Utf8Bytes` types are incompatible even though they're structurally identical. The conversion currently allocates and copies for every message.

#### Cause

axum 0.8 uses tungstenite 0.24 internally, while qmux 0.0.4 exports tungstenite 0.28. The `Utf8Bytes` and `Bytes` types across these versions are not directly convertible.

#### Possible fixes

- Upgrade axum to a version that uses tungstenite 0.28 (when available)
- Use `unsafe` transmute between identical `Bytes` layouts (not recommended)
- Bypass axum's WebSocket layer entirely and handle the upgrade manually with matching tungstenite version

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## Closes

- [#1095](https://github.com/moq-dev/moq/issues/1095) - close this issue when the quest finishes
