# Migrating moq-sock

How to update your code for each breaking change, newest first.

## Unreleased

### A shard group binds in one call (#5688)

`shard::Group`, `Member`, `Claim`, `cid`, and `CID_LEN` are gone.
`shard::bind(addr, count)` returns the complete `Bound` group; take sockets from it with `member()`.
Connection IDs are steered by `moq_quic::RandomConnectionIdGenerator::set_shard` instead of `shard::cid`.

```rust
let mut bound = shard::bind(addr, count)?;
while let Some(socket) = bound.member()? {
	// serve socket
}
```
