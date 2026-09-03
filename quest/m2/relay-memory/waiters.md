# [M] Waiter slots

## Goal

Every kio channel stops paying 840 B of empty inline waker slots. That is
~4.2 KB of the 8.8 KB an announced broadcast costs, ~2.9 KB of every additional
route, and ~840 B of every cached group, so one struct is the largest single
lever in this questline.

## Plan

### The cost

`WaiterList` is a `SmallVec<[Weak<Waker>; INLINE_WAITERS]>` with
`INLINE_WAITERS = 32`. The inline array lives inside the enclosing
`Arc<Mutex<State<T>>>`, so it is paid whether or not anything ever parks:
`size_of::<WaiterList>()` is 280 B and `size_of::<State<()>>()` is 848 B for
three lists that are almost always empty. The doc comment ("allocating nothing
until the first `register`") describes the spill, not the inline array, and
should be corrected in the same change. 32 arrived with an unrelated refactor
(moq-dev/moq#2116) and has no benchmark behind it.

### The shape

Replace the `SmallVec` with one inline slot plus a spill vector:

```rust
pub struct WaiterList {
	/// The common case: at most one waiter parked, inline and unallocated.
	one: Option<Weak<Waker>>,
	/// Everything past the first. Unallocated until a second waiter parks.
	rest: Vec<Weak<Waker>>,
	/// Rotating cursor for opportunistic GC over `rest`.
	cursor: u32,
}
```

Prototyped and measured against a `SmallVec` capacity sweep. It ties the best
capacity on size while dropping the inline/spilled branch on every access, and
`register`'s GC gets simpler: the inline slot is probed unconditionally (better
coverage than the rotating window it replaces) and only `rest` needs the cursor.

| | `WaiterList` | `State<()>` | 1 route | 5 routes | allocations/broadcast |
|---|---|---|---|---|---|
| `SmallVec`, 32 (today) | 280 B | 848 B | 8.8 KB | 26.2 KB | 30.8 |
| `SmallVec`, 8 | 88 B | 272 B | 5.9 KB | 18.7 KB | 30.8 |
| `SmallVec`, 2 | 40 B | 128 B | 5.2 KB | 16.8 KB | 30.8 |
| `SmallVec`, 1 | 32 B | 104 B | 5.1 KB | 16.6 KB | 31.8 |
| **`Option` + `Vec`** | **40 B** | **128 B** | **5.2 KB** | **16.8 KB** | **30.8** |

A boxed enum (`Empty | One(Weak) | Many(Box<..>)`) would reach 16 B per list, but
it buys only ~7% more per broadcast and pays for it with a pointer chase on the
many-waiters path, which is exactly the fan-out hot path. Not worth it.

### Risk

The list that habitually holds more than one waiter is a hot track's
`waiters_value` under real fan-out, and it now allocates a `Vec` on the second
registration. It spilled under the old shape too, just at 32, and `Vec` does not
shrink back, so there is no allocate/free thrash.

`cargo test -p kio` (47) and `cargo test -p moq-net` (766) already pass on the
prototype. kio's own tests reach into `WaiterList::entries`, so they need a
`#[cfg(test)]` slot accessor or a rewrite against the public surface.

Before the PR, run `cargo bench -p moq-net` (the `group_*` benches) on both
shapes and put the comparison in the description. No-throughput-regression is
the claim reviewers will want evidence for, and the fan-out path is where it
would show.
