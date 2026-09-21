# moq-sock

Socket and thread-per-core listener plumbing shared by the native MoQ
runtimes (`moq-tokio` and `moq-uring`).

- **`bind`**: dual-stack UDP/TCP binding with grown socket buffers, and
  `SO_REUSEPORT` for worker-per-core groups (Linux-only, refused loudly
  elsewhere).
- **`shard`**: forming and steering a reuseport group by QUIC connection id.
  A `shard::Group` holds the port (probing and locking against a second
  same-UID group), fixes the member count, and hands out one `Member` per slot
  in index order. Binding a member yields an opaque claim, and completing the
  group with every claim attaches a classic-BPF filter before any socket is
  released for serving. The completed group retains every member socket, so
  dropping one serving handle cannot renumber the survivors. A `Shard` names
  the slot a member ended up in, and `cid_prefix` is the byte its issued ids
  lead with.
- **`cpu`**: pinning worker threads to cores.

This is infrastructure, not an entry point: build against `moq-tokio` or
`moq-uring`, which own the worker groups formed from these pieces.
