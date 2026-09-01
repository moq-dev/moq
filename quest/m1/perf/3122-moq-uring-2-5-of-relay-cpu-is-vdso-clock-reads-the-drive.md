# [M] moq-uring: ~2.5% of relay CPU is vdso clock reads; the drive loop and its callers each re-read Instant::now()

## Goal

Implement and verify the behavior tracked in [#3122](https://github.com/moq-dev/moq/issues/3122)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Profiling the io\_uring relay (`dev` @ `fc57e0175`, `perf record -F 499`, relay process only) shows `[vdso]` as a top-5 DSO, at roughly 3x its share on the tokio worker path:

| DSO | video, io\_uring | video, tokio workers | chat, io\_uring |
|---|---|---|---|
| `moq-relay` | 65.89% | 68.61% | 68.96% |
| `[kernel.kallsyms]` | 22.64% | 22.97% | 20.49% |
| `libc.so.6` | 5.74% | 4.48% | 6.03% |
| **`[vdso]`** | **2.95%** | **0.72%** | **2.55%** |

That is `clock_gettime`. Roughly 2.5% of relay CPU spent reading the clock.

#### Where

The drive loop reads it once per turn:

```rust
// rs/moq-uring/src/worker.rs, block_on
self.shared.timers.borrow_mut().fire(Instant::now());
```

and then callers read it again independently on the same turn, e.g.:

```rust
// rs/moq-uring/src/quic/connection.rs
fn arm_keep_alive(&mut self) {
    let at = self.keep_alive_every
        .and_then(|every| std::time::Instant::now().checked_add(every));
    self.keep_alive.set(at);
}
```

plus quiche's own `Instant::now()` calls inside `on_timeout` / `timeout`. Because the driver re-polls per GSO train rather than per ingress event (#3120), each of those turns pays for its own clock reads.

The same profile also shows the timer heap itself at ~1.6%: `<moq_uring::timer::Timer as moq_net::runtime::Timer>::set` 0.92% plus `btree::search::search_tree` 0.65%. `timer::Heap` is a `BTreeMap<(Instant, u64), Rc<Slot>>`, so every QUIC timeout re-arm is an O(log n) map removal and insertion with `Rc` traffic. The design note in #2875 called for a timer wheel here; the landed implementation is the BTreeMap. Not urgent at these connection counts, but it is on the same hot path and grows with it.

#### Suggestion

`moq_net::runtime::Runtime` already carries a defaulted `now()`, which is a natural place to hand the current turn's instant down instead of having each layer re-read it. Sampling once per drive turn and passing it through `fire`, the keep-alive arming, and the quiche timeout calls should recover most of that 2.5%.

## Closes

- [#3122](https://github.com/moq-dev/moq/issues/3122) - close this issue when the quest finishes
