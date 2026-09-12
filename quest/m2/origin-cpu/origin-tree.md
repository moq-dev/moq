# [M] Flatten the origin local tree

## Goal

Exact-path create/resolve is not linear in path depth times mutex plus
string hash. Memory per node drops. Prune-on-empty and identity check on
remove stay.

## Plan

`OriginNode` nests `HashMap<String, Lock<OriginNode>>`. `resolve_broadcast`
locks each segment. `entry` does `dir.to_string()` on insert. A path of
depth D pays D mutexes and D string hashes, while the full path is already
an interned `Arc<str>` on `PathOwned`. Local ingest (`create_broadcast`)
and exact-path resolve walk this before the route table.

One lock on the tree, or a concurrent map keyed by interned segment / full
path. Store `Arc<str>` (or an index into the parent `Path`) instead of
owned `String`.

Independently completable from [origin-index](/quest/m2/origin-cpu/origin-index.md)
but both edit `origin.rs`; rebase rather than combine unless they land
together.

Acceptance: same new `origin.rs` Criterion: create/resolve D-segment paths,
N = 10k–100k, depth 2–8. Resolve CPU not linear in D×lock.

## Related

- [Index the origin route table](/quest/m2/origin-cpu/origin-index.md) - advertised routes, not this tree
- [Relay memory](/quest/m2/relay-memory.md) - `RouteEntry` / `ServeState`, not `OriginNode`
