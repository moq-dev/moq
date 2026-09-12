# Origin lookup CPU

## Goal

Announce, subscribe, and exact-path resolve stay cheap as the live
advertisement set grows. Chat (`moq-bench/config/announce.toml`) and a
cluster mesh (one cursor per peer) are the shapes this must not explode on.

[Relay memory](/quest/m2/relay-memory.md) is bytes per announcement. This
line is lookup CPU.

## Quests

- [Index the origin route table](/quest/m2/origin-cpu/origin-index.md) - `best_route` and cursor sync stop scanning every live advertisement
- [Origin local tree](/quest/m2/origin-cpu/origin-tree.md) - exact-path resolve does not lock and hash every path segment
