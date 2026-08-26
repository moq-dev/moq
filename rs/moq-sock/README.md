# moq-sock

Socket and thread-per-core listener plumbing shared by the native MoQ
runtimes (`moq-tokio` and `moq-uring`).

- **`bind`**: dual-stack UDP/TCP binding with grown socket buffers, and
  `SO_REUSEPORT` for worker-per-core groups (Linux-only, refused loudly
  elsewhere).
- **`shard`**: forming and steering a reuseport group by QUIC connection id.
  A `Shard` names one member's slot; `shard::bind` binds the group in order
  (probing and port-locking against a second same-UID group) and attaches a
  classic-BPF filter that steers each packet by the first byte of its
  destination connection id; `cid_prefix` is the byte a member's issued ids
  lead with.
- **`cpu`**: pinning worker threads to cores.

This is infrastructure, not an entry point: build against `moq-tokio` or
`moq-uring`, which own the worker groups formed from these pieces.
