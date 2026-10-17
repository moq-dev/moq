# Migrating moq-quic

How to update your code for each breaking change, newest first.

## Unreleased

### `congestion::NewReno` removed

`congestion::NewReno` and `congestion::NewRenoConfig` are gone.
Use `Bbr3Config` (the default) or `CubicConfig` instead.

```rust
// Before
config.congestion_controller_factory(Arc::new(congestion::NewRenoConfig::default()));

// After
config.congestion_controller_factory(Arc::new(congestion::CubicConfig::default()));
```
