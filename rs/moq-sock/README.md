# moq-sock

Socket and thread-per-core listener plumbing shared by the native MoQ
runtimes (`moq-tokio` and `moq-uring`).

- **`bind`**: dual-stack UDP/TCP binding with grown socket buffers, and
  `SO_REUSEPORT` for worker-per-core groups (Linux-only, refused loudly
  elsewhere).
- **`shard`**: forming and steering a reuseport group by QUIC connection id.
  A `shard::Group` holds the port (probing and locking against a second
  same-UID group), fixes the member count, and hands out one `Member` per slot
  in index order; binding a member joins the group, and the last one attaches a
  classic-BPF filter that steers each packet by the first byte of its
  destination connection id. A `Shard` names the slot a member ended up in, and
  `cid_prefix` is the byte its issued ids lead with.
- **`cpu`**: pinning worker threads to cores.

This is infrastructure, not an entry point: build against `moq-tokio` or
`moq-uring`, which own the worker groups formed from these pieces.
